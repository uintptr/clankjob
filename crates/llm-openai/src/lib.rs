//! LLM provider for OpenAI-compatible Chat Completions APIs (design §8).
//!
//! One adapter covers OpenAI and every server that speaks the same API (Ollama, vLLM,
//! LM Studio, OpenRouter, …); only the base URL, API key and model change.

use std::time::Duration;

use clankjob_core::llm::{
    AssistantMessage, CompletionRequest, CompletionResponse, LlmError, LlmProvider, Message, ModelInfo, TokenUsage,
    ToolCall,
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
    /// Whether the model can be shown images.
    pub vision: bool,
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
            content: UserContent<'a>,
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

    /// Plain text, or text plus images for vision models.
    #[derive(Serialize)]
    #[serde(untagged)]
    pub enum UserContent<'a> {
        Text(&'a str),
        Parts(Vec<Part<'a>>),
    }

    #[derive(Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum Part<'a> {
        Text { text: &'a str },
        ImageUrl { image_url: ImageUrl },
    }

    #[derive(Serialize)]
    pub struct ImageUrl {
        /// A `data:` URL carrying the image itself.
        pub url: String,
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

    /// `GET /models`. OpenAI and Ollama only send `id`; OpenRouter adds the rest.
    #[derive(Deserialize)]
    pub struct Models {
        pub data: Vec<Model>,
    }

    #[derive(Deserialize)]
    pub struct Model {
        pub id: String,
        #[serde(default)]
        pub name: Option<String>,
        #[serde(default)]
        pub context_length: Option<u64>,
        #[serde(default)]
        pub pricing: Option<Pricing>,
        #[serde(default)]
        pub supported_parameters: Option<Vec<String>>,
    }

    /// US dollars per token, as decimal strings.
    #[derive(Deserialize)]
    pub struct Pricing {
        #[serde(default)]
        pub prompt: Option<String>,
        #[serde(default)]
        pub completion: Option<String>,
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
            Message::User { text, images } if images.is_empty() => wire::RequestMessage::User {
                content: wire::UserContent::Text(text),
            },
            Message::User { text, images } => {
                let mut parts = Vec::with_capacity(images.len().saturating_add(1));
                parts.push(wire::Part::Text { text });
                parts.extend(images.iter().map(|image| wire::Part::ImageUrl {
                    image_url: wire::ImageUrl {
                        url: format!("data:{};base64,{}", image.media_type, image.base64),
                    },
                }));
                wire::RequestMessage::User {
                    content: wire::UserContent::Parts(parts),
                }
            }
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

/// Convert a per-token price string to dollars per million tokens.
///
/// Negative prices mean "variable" on OpenRouter and are treated as unknown.
fn per_million(price: Option<&str>) -> Option<f64> {
    price
        .and_then(|price| price.parse::<f64>().ok())
        .filter(|price| *price >= 0.0)
        // Rounded to 6 decimals so "0.0000004" becomes 0.4 and not 0.39999999999999997.
        .map(|price| (price * 1_000_000_000_000.0).round() / 1_000_000.0)
}

/// Keep the models that can call tools, when the provider says which ones can.
fn models_from_wire(models: wire::Models) -> Vec<ModelInfo> {
    let mut models: Vec<ModelInfo> = models
        .data
        .into_iter()
        .filter(|model| {
            model
                .supported_parameters
                .as_ref()
                .is_none_or(|parameters| parameters.iter().any(|p| p == "tools"))
        })
        .map(|model| ModelInfo {
            input_price: per_million(model.pricing.as_ref().and_then(|pricing| pricing.prompt.as_deref())),
            output_price: per_million(model.pricing.as_ref().and_then(|pricing| pricing.completion.as_deref())),
            id: model.id,
            name: model.name,
            context_length: model.context_length,
        })
        .collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models
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

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.config.base_url.trim_end_matches('/'))
    }

    fn authorize<B>(&self, builder: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.config.api_key {
            Some(key) => builder.header("Authorization", format!("Bearer {}", key.expose_secret())),
            None => builder,
        }
    }
}

/// Read a response body, turning error statuses into an [`LlmError`].
fn success_body(mut response: ureq::http::Response<ureq::Body>) -> Result<String, LlmError> {
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| LlmError::Retryable(format!("cannot read response: {error}")))?;
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(http_error(status, &body))
    }
}

impl LlmProvider for OpenAiCompatible {
    fn default_model(&self) -> &str {
        &self.config.model
    }

    fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
        // Network failures (refused, reset, timeout, DNS) are all worth retrying.
        let response = self
            .authorize(self.agent.post(self.url("chat/completions")))
            .send_json(to_wire(request))
            .map_err(|error| LlmError::Retryable(format!("request failed: {error}")))?;
        let body = success_body(response)?;
        let parsed: wire::Response =
            serde_json::from_str(&body).map_err(|error| LlmError::Fatal(format!("unexpected response: {error}")))?;
        from_wire(parsed)
    }

    fn supports_images(&self) -> bool {
        self.config.vision
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        let response = self
            .authorize(self.agent.get(self.url("models")))
            .call()
            .map_err(|error| LlmError::Retryable(format!("request failed: {error}")))?;
        let body = success_body(response)?;
        let parsed: wire::Models =
            serde_json::from_str(&body).map_err(|error| LlmError::Fatal(format!("unexpected response: {error}")))?;
        Ok(models_from_wire(parsed))
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
                let received_body = if raw.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_str(&raw).unwrap()
                };
                log.lock().unwrap().push((auth, received_body));
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
                vision: false,
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
                    images: Vec::new(),
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
    fn images_are_sent_as_data_url_parts() {
        let mut with_image = request();
        with_image.messages.push(Message::User {
            text: "Image `panel.png`:".to_owned(),
            images: vec![clankjob_core::llm::ImageData {
                media_type: "image/png".to_owned(),
                base64: "iVBORw==".to_owned(),
            }],
        });

        let body = serde_json::to_value(to_wire(&with_image)).unwrap();

        assert_eq!(body["messages"][1]["content"], "Start.");
        assert_eq!(
            body["messages"][4]["content"],
            json!([
                {"type": "text", "text": "Image `panel.png`:"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw=="}}
            ])
        );
    }

    #[test]
    fn no_api_key_sends_no_authorization_and_text_replies_parse() {
        let server = TestServer::start(200, r#"{"choices": [{"message": {"content": "Hello"}}]}"#);

        let response = server.provider(None).complete(&request()).unwrap();

        assert_eq!(server.received.lock().unwrap()[0].0, None);
        assert_eq!(response.message.text.as_deref(), Some("Hello"));
        assert_eq!(response.message.tool_calls, [] as [clankjob_core::llm::ToolCall; 0]);
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
            vision: false,
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
    fn openrouter_models_are_filtered_to_tool_callers_with_prices() {
        // Arrange: OpenRouter's shape, with one model that cannot call tools.
        let server = TestServer::start(
            200,
            r#"{"data": [
                {"id": "z/tools", "name": "Z", "context_length": 128000,
                 "pricing": {"prompt": "0.0000004", "completion": "0.0000016"},
                 "supported_parameters": ["tools", "temperature"]},
                {"id": "a/variable", "pricing": {"prompt": "-1", "completion": "-1"},
                 "supported_parameters": ["tools"]},
                {"id": "m/no-tools", "supported_parameters": ["temperature"]}
            ]}"#,
        );

        // Act
        let models = server.provider(Some("sk-or")).list_models().unwrap();

        // Assert
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, ["a/variable", "z/tools"]);
        assert_eq!(models[0].input_price, None);
        let priced = &models[1];
        assert_eq!(priced.input_price, Some(0.4));
        assert_eq!(priced.output_price, Some(1.6));
        assert_eq!(priced.context_length, Some(128_000));
        assert_eq!(server.received.lock().unwrap()[0].0.as_deref(), Some("Bearer sk-or"));
    }

    #[test]
    fn plain_openai_model_lists_are_kept_whole() {
        let server = TestServer::start(
            200,
            r#"{"object": "list", "data": [{"id": "llama3", "object": "model"}]}"#,
        );

        let models = server.provider(None).list_models().unwrap();

        assert_eq!(models, vec![ModelInfo::from_id("llama3")]);
    }

    #[test]
    fn model_listing_errors_are_reported() {
        let server = TestServer::start(401, r#"{"error": {"message": "bad key"}}"#);

        assert_eq!(
            server.provider(None).list_models().unwrap_err(),
            LlmError::Fatal("HTTP 401: bad key".to_owned())
        );
    }

    #[test]
    fn base_url_trailing_slash_is_ignored() {
        let provider = OpenAiCompatible::new(OpenAiConfig {
            base_url: "http://localhost:11434/v1/".to_owned(),
            api_key: None,
            model: "llama".to_owned(),
            timeout: Duration::from_secs(1),
            vision: true,
        });

        assert_eq!(
            provider.url("chat/completions"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(provider.default_model(), "llama");
        assert!(provider.supports_images());
    }
}
