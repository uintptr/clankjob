//! REST API (design §14), served by rouille.
//!
//! Handlers only read and write the database through the engine; they never call the LLM,
//! so request threads always return quickly.

use chrono::Utc;
use clankjob_core::case::{Budgets, CaseState, NewCase};
use clankjob_core::human::HumanRequestStatus;
use clankjob_core::ids::{CaseId, HumanRequestId};
use clankjob_engine::{Engine, EngineError};
use clankjob_storage::cases::CaseFilter;
use clankjob_storage::human::Answer;
use clankjob_storage::{self as storage, Connection, StorageError};
use rouille::{Request, Response, router};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};

/// Default and maximum page sizes for list endpoints.
const DEFAULT_PAGE: u32 = 50;
const MAX_PAGE: u32 = 1000;

/// An error returned to the API client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// The request is malformed or refers to unknown configuration (400).
    BadRequest(String),
    /// Missing or wrong bearer token (401).
    Unauthorized,
    /// The resource does not exist (404).
    NotFound(String),
    /// The operation conflicts with the resource's current state (409).
    Conflict(String),
    /// Something failed on the server (500); details are logged, not returned.
    Internal,
}

impl AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::BadRequest(message) => (400, "bad_request", message),
            Self::Unauthorized => (401, "unauthorized", "missing or invalid bearer token".to_owned()),
            Self::NotFound(message) => (404, "not_found", message),
            Self::Conflict(message) => (409, "conflict", message),
            Self::Internal => (500, "internal", "internal server error".to_owned()),
        };
        Response::json(&json!({ "error": { "code": code, "message": message } })).with_status_code(status)
    }
}

impl From<StorageError> for AppError {
    fn from(error: StorageError) -> Self {
        tracing::error!(%error, "storage error while handling a request");
        Self::Internal
    }
}

impl From<EngineError> for AppError {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::CaseNotFound(_) | EngineError::RequestNotFound(_) => Self::NotFound(error.to_string()),
            EngineError::AlreadyResolved(_) | EngineError::InvalidState { .. } => Self::Conflict(error.to_string()),
            EngineError::UnknownLlm(_) | EngineError::UnknownProfile(_) => Self::BadRequest(error.to_string()),
            EngineError::Storage(error) => error.into(),
            EngineError::Prompt(error) => {
                tracing::error!(%error, "prompt error while handling a request");
                Self::Internal
            }
        }
    }
}

type Handled = Result<Response, AppError>;

/// Defaults applied to cases created through the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseDefaults {
    /// LLM for cases that name none.
    pub llm: String,
    /// Profile for cases that name none.
    pub profile: Option<String>,
    /// Budgets for cases that set none.
    pub budgets: Budgets,
}

/// Everything request handlers need.
pub struct AppState {
    engine: Engine,
    tokens: Vec<SecretString>,
    defaults: CaseDefaults,
}

impl AppState {
    /// Bundle the engine, accepted API tokens and case defaults.
    #[must_use]
    pub fn new(engine: Engine, tokens: Vec<SecretString>, defaults: CaseDefaults) -> Self {
        Self {
            engine,
            tokens,
            defaults,
        }
    }

    fn connect(&self) -> Result<Connection, AppError> {
        Ok(self.engine.db().connect()?)
    }
}

/// Compare two byte strings in time that depends only on their lengths, so an attacker
/// cannot guess a token byte by byte from response times.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0_u8, |difference, (a, b)| difference | (a ^ b)) == 0
}

fn authorized(state: &AppState, request: &Request) -> bool {
    let Some(token) = request
        .header("Authorization")
        .and_then(|header| header.strip_prefix("Bearer "))
    else {
        return false;
    };
    // `fold` instead of `any` so every token is compared, whichever one matches.
    state.tokens.iter().fold(false, |found, accepted| {
        constant_time_eq(accepted.expose_secret().as_bytes(), token.as_bytes()) | found
    })
}

fn json_body<T>(request: &Request) -> Result<T, AppError>
where
    T: serde::de::DeserializeOwned,
{
    rouille::input::json_input(request).map_err(|error| AppError::BadRequest(format!("invalid JSON body: {error}")))
}

fn page_size(request: &Request) -> Result<u32, AppError> {
    request.get_param("limit").map_or(Ok(DEFAULT_PAGE), |limit| {
        limit
            .parse::<u32>()
            .ok()
            .filter(|limit| (1..=MAX_PAGE).contains(limit))
            .ok_or_else(|| AppError::BadRequest(format!("`limit` must be between 1 and {MAX_PAGE}")))
    })
}

fn parse_param<T>(request: &Request, name: &str) -> Result<Option<T>, AppError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    request
        .get_param(name)
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|error| AppError::BadRequest(format!("invalid `{name}`: {error}")))
        })
        .transpose()
}

fn health(state: &AppState) -> Response {
    let database = state.connect().is_ok();
    let last_tick = state.engine.last_scheduler_tick();
    let scheduler = last_tick.is_some_and(|tick| Utc::now().signed_duration_since(tick) < chrono::Duration::minutes(2));
    let status = if database && scheduler { 200 } else { 503 };
    Response::json(&json!({ "database": database, "scheduler": scheduler, "last_scheduler_tick": last_tick }))
        .with_status_code(status)
}

/// Body of `POST /cases`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateCaseBody {
    title: String,
    goal: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    llm: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    budgets: Option<Budgets>,
}

fn create_case(state: &AppState, request: &Request) -> Handled {
    let body: CreateCaseBody = json_body(request)?;
    if body.title.trim().is_empty() || body.goal.trim().is_empty() {
        return Err(AppError::BadRequest("`title` and `goal` must not be empty".to_owned()));
    }
    let new_case = NewCase {
        title: body.title,
        goal: body.goal,
        owner: body.owner,
        profile: body.profile.or_else(|| state.defaults.profile.clone()),
        llm: body.llm.unwrap_or_else(|| state.defaults.llm.clone()),
        model: body.model,
        budgets: body.budgets.unwrap_or(state.defaults.budgets),
    };
    let case = state.engine.create_case(&mut state.connect()?, &new_case)?;
    Ok(Response::json(&case).with_status_code(201))
}

fn list_cases(state: &AppState, request: &Request) -> Handled {
    let filter = CaseFilter {
        state: parse_param::<CaseState>(request, "state")?,
        before: request.get_param("cursor").map(CaseId::from_string),
        limit: page_size(request)?,
    };
    let cases = storage::cases::list_cases(&state.connect()?, &filter)?;
    let full_page = u32::try_from(cases.len()).is_ok_and(|count| count == filter.limit);
    let next_cursor = cases.last().filter(|_| full_page).map(|case| case.id.clone());
    Ok(Response::json(&json!({ "cases": cases, "next_cursor": next_cursor })))
}

fn get_case(state: &AppState, id: &CaseId) -> Handled {
    let connection = state.connect()?;
    let case =
        storage::cases::get_case(&connection, id)?.ok_or_else(|| AppError::NotFound(format!("case {id} not found")))?;
    let notes = storage::notes::list_notes(&connection, id)?;
    let wait_conditions = storage::waits::active_waits(&connection, id)?;
    let human_requests = storage::human::list_requests(&connection, Some(HumanRequestStatus::Open), Some(id))?;
    Ok(Response::json(&json!({
        "case": case,
        "notes": notes,
        "wait_conditions": wait_conditions,
        "open_human_requests": human_requests,
    })))
}

fn list_events(state: &AppState, request: &Request, id: &CaseId) -> Handled {
    let connection = state.connect()?;
    if storage::cases::get_case(&connection, id)?.is_none() {
        return Err(AppError::NotFound(format!("case {id} not found")));
    }
    let after = parse_param::<i64>(request, "after")?.unwrap_or(0);
    let events = storage::events::list_events(&connection, id, after, Some(page_size(request)?))?;
    Ok(Response::json(&json!({ "events": events })))
}

/// Body of `POST /cases/{id}/messages` and `POST /human-requests/{id}/answer`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TextBody {
    text: String,
}

fn text_body(request: &Request) -> Result<String, AppError> {
    let body: TextBody = json_body(request)?;
    if body.text.trim().is_empty() {
        return Err(AppError::BadRequest("`text` must not be empty".to_owned()));
    }
    Ok(body.text)
}

fn post_message(state: &AppState, request: &Request, id: &CaseId) -> Handled {
    let text = text_body(request)?;
    state.engine.post_message(&mut state.connect()?, id, &text)?;
    Ok(Response::json(&json!({ "status": "accepted" })).with_status_code(202))
}

fn wake_case(state: &AppState, id: &CaseId) -> Handled {
    state.engine.wake_case(&mut state.connect()?, id)?;
    Ok(Response::json(&json!({ "status": "accepted" })).with_status_code(202))
}

fn cancel_case(state: &AppState, id: &CaseId) -> Handled {
    let mut connection = state.connect()?;
    state.engine.cancel_case(&mut connection, id)?;
    get_case(state, id)
}

fn list_human_requests(state: &AppState, request: &Request) -> Handled {
    // `status=all` lists every request; otherwise the default is the open ones.
    let status = match request.get_param("status").as_deref() {
        Some("all") => None,
        None => Some(HumanRequestStatus::Open),
        Some(_) => parse_param::<HumanRequestStatus>(request, "status")?,
    };
    let case_id = request.get_param("case_id").map(CaseId::from_string);
    let requests = storage::human::list_requests(&state.connect()?, status, case_id.as_ref())?;
    Ok(Response::json(&json!({ "human_requests": requests })))
}

fn answer_human_request(state: &AppState, request: &Request, id: &HumanRequestId) -> Handled {
    let text = text_body(request)?;
    let answer = Answer {
        text: &text,
        via: "web",
        responder: None,
    };
    let answered = state.engine.answer_request(&mut state.connect()?, id, answer)?;
    Ok(Response::json(&answered))
}

fn prompts_summary(prompts: &clankjob_engine::prompts::PromptSet) -> Value {
    let list: Vec<Value> = prompts
        .prompts()
        .map(|prompt| json!({ "name": prompt.name, "source": prompt.source, "hash": prompt.hash }))
        .collect();
    json!({ "prompts": list, "errors": prompts.errors() })
}

fn get_prompt(state: &AppState, name: &str) -> Handled {
    let prompts = state.engine.prompts();
    let prompt = prompts
        .get(name)
        .ok_or_else(|| AppError::NotFound(format!("prompt `{name}` not found")))?;
    Ok(Response::json(prompt))
}

fn reload(state: &AppState) -> Response {
    let prompts = state.engine.reload_prompts();
    tracing::info!(errors = prompts.errors().len(), "prompts reloaded through the API");
    Response::json(&prompts_summary(&prompts))
}

fn route(state: &AppState, request: &Request) -> Handled {
    router!(request,
        (GET) (/api/v1/cases) => { list_cases(state, request) },
        (POST) (/api/v1/cases) => { create_case(state, request) },
        (GET) (/api/v1/cases/{id: String}) => { get_case(state, &CaseId::from_string(id)) },
        (GET) (/api/v1/cases/{id: String}/events) => { list_events(state, request, &CaseId::from_string(id)) },
        (POST) (/api/v1/cases/{id: String}/messages) => { post_message(state, request, &CaseId::from_string(id)) },
        (POST) (/api/v1/cases/{id: String}/wake) => { wake_case(state, &CaseId::from_string(id)) },
        (POST) (/api/v1/cases/{id: String}/cancel) => { cancel_case(state, &CaseId::from_string(id)) },
        (GET) (/api/v1/human-requests) => { list_human_requests(state, request) },
        (POST) (/api/v1/human-requests/{id: String}/answer) => {
            answer_human_request(state, request, &HumanRequestId::from_string(id))
        },
        (GET) (/api/v1/prompts) => { Ok(Response::json(&prompts_summary(&state.engine.prompts()))) },
        (GET) (/api/v1/prompts/profiles/{name: String}) => { get_prompt(state, &format!("profiles/{name}")) },
        (GET) (/api/v1/prompts/{name: String}) => { get_prompt(state, &name) },
        (POST) (/api/v1/admin/reload) => { Ok(reload(state)) },
        _ => Err(AppError::NotFound(format!("no route for {} {}", request.method(), request.url())))
    )
}

/// Handle one HTTP request.
///
/// `/healthz` is public; everything else requires a bearer token.
///
/// # Arguments
///
/// * `state` - Shared handler state
/// * `request` - The incoming request
///
/// # Returns
///
/// The response, with errors rendered as `{"error": {"code", "message"}}`
pub fn handle(state: &AppState, request: &Request) -> Response {
    if request.method() == "GET" && request.url() == "/healthz" {
        return health(state);
    }
    if !authorized(state, request) {
        return AppError::Unauthorized.into_response();
    }
    route(state, request).unwrap_or_else(AppError::into_response)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Read;
    use std::sync::Arc;

    use clankjob_core::llm::{CompletionRequest, CompletionResponse, LlmError, LlmProvider};
    use clankjob_engine::EngineSettings;
    use clankjob_storage::Db;
    use tempfile::TempDir;

    use super::*;

    /// A provider that is never called: API tests do not start the engine's threads.
    struct UnusedProvider;

    impl LlmProvider for UnusedProvider {
        fn default_model(&self) -> &'static str {
            "unused"
        }

        fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
            Err(LlmError::Fatal("not expected in API tests".to_owned()))
        }
    }

    struct TestApi {
        state: AppState,
        _dir: TempDir,
    }

    impl TestApi {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = Db::new(dir.path().join("test.db"));
            db.migrate().unwrap();
            let providers = HashMap::from([("default".to_owned(), Arc::new(UnusedProvider) as Arc<dyn LlmProvider>)]);
            let engine = Engine::new(db, providers, None, EngineSettings::default());
            let defaults = CaseDefaults {
                llm: "default".to_owned(),
                profile: None,
                budgets: Budgets::default(),
            };
            let state = AppState::new(engine, vec![SecretString::from("token".to_owned())], defaults);
            Self { state, _dir: dir }
        }

        fn call(&self, method: &str, url: &str, body: Option<Value>) -> (u16, Value) {
            let headers = vec![
                ("Authorization".to_owned(), "Bearer token".to_owned()),
                ("Content-Type".to_owned(), "application/json".to_owned()),
            ];
            let body = body.map(|body| body.to_string().into_bytes()).unwrap_or_default();
            let request = Request::fake_http(method, url, headers, body);
            let response = handle(&self.state, &request);
            let (mut reader, _) = response.data.into_reader_and_size();
            let mut text = String::new();
            reader.read_to_string(&mut text).unwrap();
            (response.status_code, serde_json::from_str(&text).unwrap_or(Value::Null))
        }

        fn create_case(&self) -> String {
            let (status, case) = self.call(
                "POST",
                "/api/v1/cases",
                Some(json!({"title": "Quote", "goal": "Get one"})),
            );
            assert_eq!(status, 201);
            case["id"].as_str().unwrap().to_owned()
        }
    }

    #[test]
    fn requests_without_a_valid_token_are_rejected() {
        let api = TestApi::new();

        for headers in [vec![], vec![("Authorization".to_owned(), "Bearer wrong".to_owned())]] {
            let response = handle(
                &api.state,
                &Request::fake_http("GET", "/api/v1/cases", headers, Vec::new()),
            );
            assert_eq!(response.status_code, 401);
        }
    }

    #[test]
    fn health_is_public_and_reports_the_scheduler() {
        let api = TestApi::new();

        let response = handle(&api.state, &Request::fake_http("GET", "/healthz", vec![], Vec::new()));

        // The scheduler thread is not started in tests, so the check reports it down.
        assert_eq!(response.status_code, 503);
    }

    #[test]
    fn created_case_is_listed_and_detailed() {
        // Arrange
        let api = TestApi::new();
        let id = api.create_case();

        // Act
        let (list_status, list) = api.call("GET", "/api/v1/cases?state=pending", None);
        let (detail_status, detail) = api.call("GET", &format!("/api/v1/cases/{id}"), None);
        let (events_status, events) = api.call("GET", &format!("/api/v1/cases/{id}/events"), None);

        // Assert
        assert_eq!((list_status, detail_status, events_status), (200, 200, 200));
        assert_eq!(list["cases"][0]["id"], id.as_str());
        assert_eq!(detail["case"]["state"], "pending");
        assert_eq!(detail["case"]["budgets"]["max_activations"], 20);
        assert_eq!(events["events"][0]["kind"], "wake");
        assert_eq!(events["events"][0]["payload"]["reason"], "created");
    }

    #[test]
    fn invalid_input_gets_a_400_with_an_error_body() {
        let api = TestApi::new();

        let (empty_status, error) = api.call("POST", "/api/v1/cases", Some(json!({"title": "", "goal": "x"})));
        let (unknown_llm, _) = api.call(
            "POST",
            "/api/v1/cases",
            Some(json!({"title": "t", "goal": "g", "llm": "nope"})),
        );
        let (bad_state, _) = api.call("GET", "/api/v1/cases?state=asleep", None);
        let (bad_limit, _) = api.call("GET", "/api/v1/cases?limit=0", None);

        assert_eq!(empty_status, 400);
        assert_eq!(error["error"]["code"], "bad_request");
        assert_eq!((unknown_llm, bad_state, bad_limit), (400, 400, 400));
    }

    #[test]
    fn unknown_case_and_route_are_404() {
        let api = TestApi::new();

        assert_eq!(api.call("GET", "/api/v1/cases/nope", None).0, 404);
        assert_eq!(
            api.call("POST", "/api/v1/cases/nope/messages", Some(json!({"text": "hi"}))).0,
            404
        );
        assert_eq!(api.call("GET", "/api/v1/nothing", None).0, 404);
    }

    #[test]
    fn cancel_then_wake_conflicts() {
        let api = TestApi::new();
        let id = api.create_case();

        let (cancel_status, cancelled) = api.call("POST", &format!("/api/v1/cases/{id}/cancel"), None);
        let (wake_status, _) = api.call("POST", &format!("/api/v1/cases/{id}/wake"), None);
        let (message_status, _) = api.call(
            "POST",
            &format!("/api/v1/cases/{id}/messages"),
            Some(json!({"text": "hi"})),
        );

        assert_eq!(cancel_status, 200);
        assert_eq!(cancelled["case"]["state"], "cancelled");
        assert_eq!((wake_status, message_status), (409, 409));
    }

    #[test]
    fn human_request_is_answered_once() {
        // Arrange: a case waiting for an answer
        let api = TestApi::new();
        let id = api.create_case();
        let connection = api.state.connect().unwrap();
        let case_id = CaseId::from_string(id.as_str());
        let request_id = HumanRequestId::generate();
        storage::human::insert_request(&connection, &request_id, &case_id, "Photo?", Utc::now()).unwrap();
        storage::cases::update_state(&connection, &case_id, CaseState::WaitingForHuman, Utc::now()).unwrap();

        // Act
        let (_, open) = api.call("GET", "/api/v1/human-requests", None);
        let url = format!("/api/v1/human-requests/{request_id}/answer");
        let (first, answered) = api.call("POST", &url, Some(json!({"text": "Sent it"})));
        let (second, _) = api.call("POST", &url, Some(json!({"text": "Again"})));

        // Assert
        assert_eq!(open["human_requests"][0]["question"], "Photo?");
        assert_eq!(first, 200);
        assert_eq!(answered["answered_via"], "web");
        assert_eq!(second, 409);
    }

    #[test]
    fn prompts_are_listed_and_readable() {
        let api = TestApi::new();

        let (status, list) = api.call("GET", "/api/v1/prompts", None);
        let (prompt_status, prompt) = api.call("GET", "/api/v1/prompts/system", None);
        let (reload_status, _) = api.call("POST", "/api/v1/admin/reload", None);

        assert_eq!((status, prompt_status, reload_status), (200, 200, 200));
        assert_eq!(list["prompts"].as_array().unwrap().len(), 4);
        assert_eq!(prompt["source"], "builtin");
        assert_eq!(api.call("GET", "/api/v1/prompts/profiles/none", None).0, 404);
    }

    #[test]
    fn constant_time_eq_compares_content_and_length() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
