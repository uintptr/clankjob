//! LLM provider for OpenAI-compatible Chat Completions APIs (design §8).
//!
//! One adapter covers OpenAI and every server that speaks the same API (Ollama, vLLM,
//! LM Studio, OpenRouter, …); only the base URL, API key and model change.

use std::time::Duration;

use clankjob_core::llm::{
    AssistantMessage, CompletionRequest, CompletionResponse, LlmError, LlmProvider, Message, TokenUsage, ToolCall,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

/// Longest excerpt of an error response body included in error messages.
const ERROR_BODY_EXCERPT: usize = 500;

/// Connection settings for an OpenAI-compatible endpoint.
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// API base URL, e.g. `https://api.openai.com/v1` or `http://localhost:11434/v1`.
    pub base_url: String,
    /// Bearer token; `None` for local servers that need no key.
    pub api_key: Option<SecretString>,
    /// Model used when a case does not override it.
    pub model: String,
    /// Maximum time for one completion request.
    pub timeout: Duration,
}

/// Wire format of the request body.
mod wire {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize)]
    pub struct Request<'a> {
        pub model: &'a str,
        pub messages: Vec<RequestMessage<'a>>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        pub tools: Vec<Tool<'a>>,
    }

    #[derive(Serialize)]
    #[serde(tag = "role", rename_all = "snake_case")]
    pub enum RequestMessage<'a> {
        System {
            content: &'a str,
        },
        User {
            content: &'a str,
        },
        Assistant {
            content: Option<&'a str>,
            // Some servers reject an empty `tool_calls` array, so it is omitted instead.
            #[serde(skip_serializing_if = "Vec::is_empty")]
            tool_calls: Vec<RequestToolCall<'a>>,
        },
        Tool {
            tool_call_id: &'a str,
            content: &'a str,
        },
    }

    #[derive(Serialize)]
    pub struct RequestToolCall<'a> {
        pub id: &'a str,
        pub r#type: &'static str,
        pub function: RequestFunctionCall<'a>,
    }

    #[derive(Serialize)]
    pub struct RequestFunctionCall<'a> {
        pub name: &'a str,
        /// The API expects arguments as a JSON-encoded string, not an object.
        pub arguments: String,
    }

    #[derive(Serialize)]
    pub struct Tool<'a> {
        pub r#type: &'static str,
        pub function: Function<'a>,
    }

    #[derive(Serialize)]
    pub struct Function<'a> {
        pub name: &'a str,
        pub description: &'a str,
        pub parameters: &'a serde_json::Value,
    }

    #[derive(Deserialize)]
    pub struct Response {
        pub choices: Vec<Choice>,
        #[serde(default)]
        pub usage: Option<Usage>,
    }

    #[derive(Deserialize)]
    pub struct Choice {
        pub message: ResponseMessage,
    }

    #[derive(Deserialize)]
    pub struct ResponseMessage {
        #[serde(default)]
        pub content: Option<String>,
        #[serde(default)]
        pub tool_calls: Option<Vec<ResponseToolCall>>,
    }

    #[derive(Deserialize)]
    pub struct ResponseToolCall {
        pub id: String,
        pub function: ResponseFunctionCall,
    }

    #[derive(Deserialize)]
    pub struct ResponseFunctionCall {
        pub name: String,
        #[serde(default)]
        pub arguments: String,
    }

    #[derive(Deserialize)]
    pub struct Usage {
        #[serde(default)]
        pub prompt_tokens: u64,
        #[serde(default)]
        pub completion_tokens: u64,
    }

    #[derive(Deserialize)]
    pub struct ErrorBody {
        pub error: ErrorDetail,
    }

    #[derive(Deserialize)]
    pub struct ErrorDetail {
        pub message: String,
    }
}

fn to_wire(request: &CompletionRequest) -> wire::Request<'_> {
    let mut messages = Vec::with_capacity(request.messages.len().saturating_add(1));
    messages.push(wire::RequestMessage::System {
        content: &request.system,
    });
    messages.extend(request.messages.iter().map(|message| {
        match message {
            Message::User { text } => wire::RequestMessage::User { content: text },
            Message::Assistant(assistant) => wire::RequestMessage::Assistant {
                content: assistant.text.as_deref(),
                tool_calls: assistant
                    .tool_calls
                    .iter()
                    .map(|call| wire::RequestToolCall {
                        id: &call.id,
                        r#type: "function",
                        function: wire::RequestFunctionCall {
                            name: &call.name,
                            // Arguments that arrived as invalid JSON are kept as a string and
                            // sent back verbatim; everything else is re-encoded.
                            arguments: match &call.arguments {
                                Value::String(raw) => raw.clone(),
                                other => other.to_string(),
                            },
                        },
                    })
                    .collect(),
            },
            Message::Tool { tool_call_id, content } => wire::RequestMessage::Tool { tool_call_id, content },
        }
    }));
    wire::Request {
        model: &request.model,
        messages,
        tools: request
            .tools
            .iter()
            .map(|tool| wire::Tool {
                r#type: "function",
                function: wire::Function {
                    name: &tool.name,
                    description: &tool.description,
                    parameters: &tool.parameters,
                },
            })
            .collect(),
    }
}

fn from_wire(response: wire::Response) -> Result<CompletionResponse, LlmError> {
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| LlmError::Fatal("response has no choices".to_owned()))?;
    let tool_calls = choice
        .message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|call| ToolCall {
            id: call.id,
            name: call.function.name,
            // Invalid JSON is kept as a string so the tool can report the error to the LLM.
            arguments: serde_json::from_str(&call.function.arguments).unwrap_or(Value::String(call.function.arguments)),
        })
        .collect();
    let usage = response.usage.map_or_else(TokenUsage::default, |usage| TokenUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
    });
    Ok(CompletionResponse {
        message: AssistantMessage {
            text: choice.message.content,
            tool_calls,
        },
        usage,
    })
}

/// Turn a non-success HTTP response into an [`LlmError`].
fn http_error(status: u16, body: &str) -> LlmError {
    let detail = serde_json::from_str::<wire::ErrorBody>(body).map_or_else(
        |_| body.chars().take(ERROR_BODY_EXCERPT).collect(),
        |parsed| parsed.error.message,
    );
    let message = format!("HTTP {status}: {detail}");
    match status {
        408 | 409 | 429 | 500..=599 => LlmError::Retryable(message),
        _ => LlmError::Fatal(message),
    }
}

/// An OpenAI-compatible LLM endpoint.
pub struct OpenAiCompatible {
    config: OpenAiConfig,
    agent: ureq::Agent,
}

impl OpenAiCompatible {
    /// Create a provider for one endpoint.
    ///
    /// # Arguments
    ///
    /// * `config` - Base URL, key, default model and timeout
    #[must_use]
    pub fn new(config: OpenAiConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            // Error statuses are returned as responses so their body can be read.
            .http_status_as_error(false)
            .timeout_global(Some(config.timeout))
            .build()
            .into();
        Self { config, agent }
    }

    fn url(&self) -> String {
        format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'))
    }
}

impl LlmProvider for OpenAiCompatible {
    fn default_model(&self) -> &str {
        &self.config.model
    }

    fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let mut builder = self.agent.post(self.url());
        if let Some(key) = &self.config.api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key.expose_secret()));
        }
        // Network failures (refused, reset, timeout, DNS) are all worth retrying.
        let mut response = builder
            .send_json(to_wire(request))
            .map_err(|error| LlmError::Retryable(format!("request failed: {error}")))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| LlmError::Retryable(format!("cannot read response: {error}")))?;
        if !(200..300).contains(&status) {
            return Err(http_error(status, &body));
        }
        let parsed: wire::Response =
            serde_json::from_str(&body).map_err(|error| LlmError::Fatal(format!("unexpected response: {error}")))?;
        from_wire(parsed)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    use clankjob_core::llm::ToolSpec;
    use serde_json::json;

    use super::*;

    /// Authorization header and JSON body of each request a [`TestServer`] received.
    type Received = Arc<Mutex<Vec<(Option<String>, Value)>>>;

    /// A local HTTP server that answers every request with `status` and `body`.
    struct TestServer {
        url: String,
        received: Received,
        stop: std::sync::mpsc::Sender<()>,
    }

    impl TestServer {
        fn start(status: u16, body: &'static str) -> Self {
            let received = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&received);
            let server = rouille::Server::new("127.0.0.1:0", move |request| {
                let mut raw = String::new();
                request.data().unwrap().read_to_string(&mut raw).unwrap();
                let auth = request.header("Authorization").map(str::to_owned);
                log.lock().unwrap().push((auth, serde_json::from_str(&raw).unwrap()));
                rouille::Response::from_data("application/json", body).with_status_code(status)
            })
            .unwrap();
            let url = format!("http://{}/v1", server.server_addr());
            let (_handle, stop) = server.stoppable();
            Self { url, received, stop }
        }

        fn provider(&self, api_key: Option<&str>) -> OpenAiCompatible {
            OpenAiCompatible::new(OpenAiConfig {
                base_url: self.url.clone(),
                api_key: api_key.map(|key| SecretString::from(key.to_owned())),
                model: "gpt-test".to_owned(),
                timeout: Duration::from_secs(5),
            })
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            let _ = self.stop.send(());
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "gpt-test".to_owned(),
            system: "Be useful.".to_owned(),
            messages: vec![
                Message::User {
                    text: "Start.".to_owned(),
                },
                Message::Assistant(AssistantMessage {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_owned(),
                        name: "note_set".to_owned(),
                        arguments: json!({"key": "k", "value": "v"}),
                    }],
                }),
                Message::Tool {
                    tool_call_id: "call_1".to_owned(),
                    content: "{\"saved\":\"k\"}".to_owned(),
                },
            ],
            tools: vec![ToolSpec {
                name: "complete".to_owned(),
                description: "Finish.".to_owned(),
                parameters: json!({"type": "object"}),
            }],
        }
    }

    const TOOL_CALL_RESPONSE: &str = r#"{
        "choices": [{"message": {"content": null, "tool_calls": [
            {"id": "call_2", "type": "function", "function": {"name": "complete", "arguments": "{\"summary\":\"done\"}"}},
            {"id": "call_3", "type": "function", "function": {"name": "complete", "arguments": "{oops"}}
        ]}}],
        "usage": {"prompt_tokens": 42, "completion_tokens": 7}
    }"#;

    #[test]
    fn sends_the_openai_wire_format_and_parses_tool_calls() {
        // Arrange
        let server = TestServer::start(200, TOOL_CALL_RESPONSE);
        let provider = server.provider(Some("sk-test"));

        // Act
        let response = provider.complete(&request()).unwrap();

        // Assert: request
        let received = server.received.lock().unwrap();
        let (auth, body) = &received[0];
        assert_eq!(auth.as_deref(), Some("Bearer sk-test"));
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["messages"][0], json!({"role": "system", "content": "Be useful."}));
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"key\":\"k\",\"value\":\"v\"}"
        );
        assert_eq!(
            body["messages"][3],
            json!({"role": "tool", "tool_call_id": "call_1", "content": "{\"saved\":\"k\"}"})
        );
        assert_eq!(body["tools"][0]["type"], "function");
        // Assert: response
        assert_eq!(
            response.usage,
            TokenUsage {
                input_tokens: 42,
                output_tokens: 7
            }
        );
        assert_eq!(response.message.tool_calls[0].arguments, json!({"summary": "done"}));
        assert_eq!(response.message.tool_calls[1].arguments, json!("{oops"));
    }

    #[test]
    fn no_api_key_sends_no_authorization_and_text_replies_parse() {
        let server = TestServer::start(200, r#"{"choices": [{"message": {"content": "Hello"}}]}"#);

        let response = server.provider(None).complete(&request()).unwrap();

        assert_eq!(server.received.lock().unwrap()[0].0, None);
        assert_eq!(response.message.text.as_deref(), Some("Hello"));
        assert!(response.message.tool_calls.is_empty());
        assert_eq!(response.usage, TokenUsage::default());
    }

    #[test]
    fn rate_limits_are_retryable_and_auth_errors_are_fatal() {
        let limited = TestServer::start(429, r#"{"error": {"message": "slow down"}}"#);
        let unauthorized = TestServer::start(401, "not json at all");

        let limited_error = limited.provider(None).complete(&request()).unwrap_err();
        let auth_error = unauthorized.provider(None).complete(&request()).unwrap_err();

        assert_eq!(limited_error, LlmError::Retryable("HTTP 429: slow down".to_owned()));
        assert_eq!(auth_error, LlmError::Fatal("HTTP 401: not json at all".to_owned()));
    }

    #[test]
    fn connection_failure_is_retryable() {
        // Bind then drop a listener to get a port nothing listens on.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let provider = OpenAiCompatible::new(OpenAiConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            api_key: None,
            model: "m".to_owned(),
            timeout: Duration::from_secs(2),
        });

        assert!(matches!(provider.complete(&request()), Err(LlmError::Retryable(_))));
    }

    #[test]
    fn malformed_success_response_is_fatal() {
        let server = TestServer::start(200, r#"{"choices": []}"#);

        assert_eq!(
            server.provider(None).complete(&request()).unwrap_err(),
            LlmError::Fatal("response has no choices".to_owned())
        );
    }

    #[test]
    fn base_url_trailing_slash_is_ignored() {
        let provider = OpenAiCompatible::new(OpenAiConfig {
            base_url: "http://localhost:11434/v1/".to_owned(),
            api_key: None,
            model: "llama".to_owned(),
            timeout: Duration::from_secs(1),
        });

        assert_eq!(provider.url(), "http://localhost:11434/v1/chat/completions");
        assert_eq!(provider.default_model(), "llama");
    }
}
