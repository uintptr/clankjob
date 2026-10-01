//! REST API (design §14), served by rouille.
//!
//! Handlers only read and write the database through the engine; they never call the LLM,
//! so request threads always return quickly.

use chrono::Utc;
use clankjob_core::case::{ApprovalPolicy, Budgets, CaseState, ModelChange, NewCase, NewInstruction};
use clankjob_core::contact::{ContactSource, NewContact};
use clankjob_core::file::FileKind;
use clankjob_core::human::Decision;
use clankjob_core::human::HumanRequestStatus;
use clankjob_core::ids::{CaseId, ContactId, FileId, HumanRequestId, InstructionId};
use clankjob_engine::{Engine, EngineError};
use clankjob_plugin_host::{InstanceState, InstanceStatus};
use clankjob_storage::cases::CaseFilter;
use clankjob_storage::human::{Answer, Verdict};
use clankjob_storage::{self as storage, Connection, StorageError};
use std::io::Read;
use std::sync::Arc;

use rouille::{Request, Response, router};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::plugins::PluginManager;

/// Largest request body accepted, in bytes, except for file uploads.
const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;
/// Largest file upload body accepted, in bytes.
const MAX_UPLOAD_BYTES: u64 = clankjob_engine::files::MAX_FILE_BYTES as u64;

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
    /// The request body is larger than the server accepts (413).
    PayloadTooLarge,
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
            Self::PayloadTooLarge => (
                413,
                "payload_too_large",
                format!(
                    "request bodies are limited to {} MB, and file uploads to {} MB",
                    MAX_BODY_BYTES / 1024 / 1024,
                    MAX_UPLOAD_BYTES / 1024 / 1024
                ),
            ),
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
            EngineError::CaseNotFound(_) | EngineError::RequestNotFound(_) | EngineError::InstructionNotFound(_) => {
                Self::NotFound(error.to_string())
            }
            EngineError::AlreadyResolved(_) | EngineError::InvalidState { .. } => Self::Conflict(error.to_string()),
            EngineError::UnknownLlm(_)
            | EngineError::UnknownProfile(_)
            | EngineError::UnknownChannel(_)
            | EngineError::NotAQuestion(_)
            | EngineError::InvalidUserPrompt(_)
            | EngineError::NotAnApproval(_)
            | EngineError::InvalidFile(_)
            | EngineError::InvalidInstruction(_) => Self::BadRequest(error.to_string()),
            EngineError::FileStorage(error) => {
                tracing::error!(%error, "file storage error while handling a request");
                Self::Internal
            }
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
    /// Accepted bearer tokens, or `None` when no token is required.
    tokens: Option<Vec<SecretString>>,
    defaults: CaseDefaults,
    catalog: Arc<Catalog>,
    plugins: Arc<PluginManager>,
}

impl AppState {
    /// Bundle the engine, accepted API tokens (`None`: no token required), case defaults,
    /// the model catalog and the plugins.
    #[must_use]
    pub fn new(
        engine: Engine,
        tokens: Option<Vec<SecretString>>,
        defaults: CaseDefaults,
        catalog: Arc<Catalog>,
        plugins: Arc<PluginManager>,
    ) -> Self {
        Self {
            engine,
            tokens,
            defaults,
            catalog,
            plugins,
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
    let Some(tokens) = &state.tokens else {
        return true;
    };
    let Some(token) = request
        .header("Authorization")
        .and_then(|header| header.strip_prefix("Bearer "))
    else {
        return false;
    };
    // `fold` instead of `any` so every token is compared, whichever one matches.
    tokens.iter().fold(false, |found, accepted| {
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
    // `token_required` tells the web UI whether to show its sign-in page; `version` and
    // `commit` are what the server was built from (build.rs).
    Response::json(&json!({
        "version": env!("CLANKJOB_VERSION"),
        "commit": env!("CLANKJOB_COMMIT"),
        "database": database,
        "scheduler": scheduler,
        "last_scheduler_tick": last_tick,
        "token_required": state.tokens.is_some(),
    }))
    .with_status_code(status)
}

/// Body of `POST /cases`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateCaseBody {
    title: String,
    /// Without one, the case is a draft that starts with the owner's first message.
    #[serde(default)]
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
    #[serde(default)]
    instructions: Vec<InstructionBody>,
    /// Channels besides the web UI; omitted uses the server's default, `[]` means none.
    #[serde(default)]
    human_channels: Option<Vec<String>>,
    /// When approval-gated calls wait for the owner (default: unless the tool finds
    /// nothing to approve, e.g. an email to trusted contacts).
    #[serde(default)]
    approvals: ApprovalPolicy,
}

/// An instruction in `POST /cases`, `POST` or `PUT /cases/{id}/instructions`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstructionBody {
    name: String,
    content: String,
}

impl InstructionBody {
    fn into_new(self) -> NewInstruction {
        NewInstruction {
            name: self.name.trim().to_owned(),
            content: self.content,
        }
    }
}

fn create_case(state: &AppState, request: &Request) -> Handled {
    let body: CreateCaseBody = json_body(request)?;
    if body.title.trim().is_empty() {
        return Err(AppError::BadRequest("`title` must not be empty".to_owned()));
    }
    let new_case = NewCase {
        title: body.title,
        goal: body.goal,
        owner: body.owner,
        profile: body.profile.or_else(|| state.defaults.profile.clone()),
        llm: body.llm.unwrap_or_else(|| state.defaults.llm.clone()),
        model: body.model,
        budgets: body.budgets.unwrap_or(state.defaults.budgets),
        instructions: body.instructions.into_iter().map(InstructionBody::into_new).collect(),
        human_channels: body.human_channels,
        approvals: body.approvals,
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
    // Metadata only; extracted text and bytes are fetched one file at a time.
    let files = storage::files::list_files(&connection, id)?;
    let instructions = storage::instructions::list_instructions(&connection, id)?;
    Ok(Response::json(&json!({
        "case": case,
        "notes": notes,
        "wait_conditions": wait_conditions,
        "open_human_requests": human_requests,
        "instructions": instructions,
        "files": files,
        "cost": crate::catalog::cost_estimate(&case.usage, state.catalog.model(&case.llm, case.model.as_deref())),
    })))
}

/// Add (`id` unset), edit, or remove (`body` unset) an instruction.
fn change_instruction(
    state: &AppState,
    request: Option<&Request>,
    case_id: &CaseId,
    id: Option<&InstructionId>,
) -> Handled {
    let instruction = request
        .map(json_body::<InstructionBody>)
        .transpose()?
        .map(InstructionBody::into_new);
    let stored = state
        .engine
        .change_instruction(&mut state.connect()?, case_id, id, instruction.as_ref())?;
    Ok(match (stored, id) {
        (Some(stored), None) => Response::json(&stored).with_status_code(201),
        (Some(stored), Some(_)) => Response::json(&stored),
        (None, _) => Response::json(&json!({ "status": "removed" })),
    })
}

fn upload_file(state: &AppState, request: &Request, case_id: &CaseId) -> Handled {
    let name = request
        .get_param("name")
        .ok_or_else(|| AppError::BadRequest("the `name` query parameter is required".to_owned()))?;
    let Some(body) = request.data() else {
        return Err(AppError::BadRequest("the request body was already read".to_owned()));
    };
    // One byte past the limit is enough to tell an oversized upload that lied about
    // (or omitted) its Content-Length.
    let mut bytes = Vec::new();
    body.take(MAX_UPLOAD_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| AppError::BadRequest(format!("cannot read the upload: {error}")))?;
    if bytes.len() as u64 > MAX_UPLOAD_BYTES {
        return Err(AppError::PayloadTooLarge);
    }
    let file = state.engine.add_file(&mut state.connect()?, case_id, &name, &bytes)?;
    Ok(Response::json(&file).with_status_code(201))
}

fn load_file(state: &AppState, case_id: &CaseId, id: &FileId) -> Result<clankjob_core::file::CaseFile, AppError> {
    storage::files::get_file(&state.connect()?, case_id, id)?
        .ok_or_else(|| AppError::NotFound(format!("file {id} not found")))
}

/// Metadata plus the extracted text, for text files and PDFs.
fn get_file(state: &AppState, case_id: &CaseId, id: &FileId) -> Handled {
    let file = load_file(state, case_id, id)?;
    let mut body = serde_json::to_value(&file).map_err(|_| AppError::Internal)?;
    if let Value::Object(fields) = &mut body {
        fields.insert("text".to_owned(), json!(file.text));
    }
    Ok(Response::json(&body))
}

/// The file's bytes. Only images are shown inline; everything else is a download, and
/// nothing is ever served as something a browser would render as a page.
fn file_content(state: &AppState, case_id: &CaseId, id: &FileId) -> Handled {
    let file = load_file(state, case_id, id)?;
    let bytes = state.engine.files().read(&file.id).map_err(|error| {
        tracing::error!(%error, file_id = %file.id, "cannot read stored file");
        AppError::Internal
    })?;
    let (content_type, disposition) = match file.kind {
        FileKind::Image => (file.media_type.clone(), "inline"),
        FileKind::Pdf => ("application/pdf".to_owned(), "attachment"),
        FileKind::Text => ("application/octet-stream".to_owned(), "attachment"),
    };
    // Quotes and control characters cannot break out of the header's filename.
    let safe_name: String = file
        .name
        .chars()
        .map(|c| {
            if c.is_control() || c == '"' || c == '\\' {
                '_'
            } else {
                c
            }
        })
        .collect();
    Ok(Response::from_data(content_type, bytes)
        .with_additional_header(
            "Content-Disposition",
            format!("{disposition}; filename=\"{safe_name}\""),
        )
        .with_additional_header("X-Content-Type-Options", "nosniff")
        .with_additional_header("Content-Security-Policy", "default-src 'none'; sandbox"))
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

/// Body of `PATCH /cases/{id}`: what to change.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateCaseBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    approvals: Option<ApprovalPolicy>,
    /// Another configured LLM.
    #[serde(default)]
    llm: Option<String>,
    /// Absent: keep the model; `null` or blank: the LLM's default; a string: that model.
    #[serde(default, deserialize_with = "model_change")]
    model: ModelChange,
    /// The chat channels it asks and notifies on; `[]` for the web client only.
    #[serde(default)]
    human_channels: Option<Vec<String>>,
}

/// `model` in `PATCH /cases/{id}`, when it is present (absent is `Keep`, by default).
fn model_change<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<ModelChange, D::Error> {
    Ok(match Option::<String>::deserialize(deserializer)? {
        Some(model) if !model.trim().is_empty() => ModelChange::Model(model.trim().to_owned()),
        _ => ModelChange::Default,
    })
}

/// Longest case title, in characters.
const MAX_TITLE_CHARS: usize = 200;

/// Longest model id, in characters.
const MAX_MODEL_CHARS: usize = 200;

fn update_case(state: &AppState, request: &Request, id: &CaseId) -> Handled {
    let body: UpdateCaseBody = json_body(request)?;
    let model_given = body.model != ModelChange::Keep;
    if body.title.is_none()
        && body.approvals.is_none()
        && body.llm.is_none()
        && !model_given
        && body.human_channels.is_none()
    {
        return Err(AppError::BadRequest(
            "nothing to change: give a `title`, `approvals`, `llm`, `model` or `human_channels`".to_owned(),
        ));
    }
    // Checked before anything changes, like the LLM below.
    let channels = body
        .human_channels
        .as_deref()
        .map(|channels| state.engine.channels().resolve(Some(channels)))
        .transpose()?;
    let title = body.title.as_deref().map(str::trim);
    if title.is_some_and(|title| title.is_empty() || title.chars().count() > MAX_TITLE_CHARS) {
        return Err(AppError::BadRequest(format!(
            "`title` must be 1 to {MAX_TITLE_CHARS} characters"
        )));
    }
    let llm = body.llm.as_deref().map(str::trim);
    if llm.is_some_and(str::is_empty) {
        return Err(AppError::BadRequest("`llm` must not be empty".to_owned()));
    }
    if let ModelChange::Model(model) = &body.model
        && model.chars().count() > MAX_MODEL_CHARS
    {
        return Err(AppError::BadRequest(format!(
            "`model` must be at most {MAX_MODEL_CHARS} characters"
        )));
    }
    let mut connection = state.connect()?;
    // First: an unknown LLM is refused before anything else changes.
    if llm.is_some() || model_given {
        state.engine.set_model(&mut connection, id, llm, &body.model)?;
    }
    if let Some(channels) = channels {
        state.engine.set_human_channels(&mut connection, id, &channels)?;
    }
    if let Some(title) = title {
        state.engine.rename_case(&mut connection, id, title)?;
    }
    if let Some(approvals) = body.approvals {
        state.engine.set_approvals(&mut connection, id, approvals)?;
    }
    get_case(state, id)
}

/// Body of `POST /contacts` and `PUT /contacts/{id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContactBody {
    name: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    phone: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    trusted: bool,
}

fn contact_body(request: &Request) -> Result<NewContact, AppError> {
    let body: ContactBody = json_body(request)?;
    clankjob_engine::contacts::clean(NewContact {
        name: body.name,
        email: body.email,
        phone: body.phone,
        note: body.note,
        trusted: body.trusted,
    })
    .map_err(AppError::BadRequest)
}

fn list_contacts(state: &AppState) -> Handled {
    let contacts = storage::contacts::list_contacts(&state.connect()?)?;
    Ok(Response::json(&json!({ "contacts": contacts })))
}

fn add_contact(state: &AppState, request: &Request) -> Handled {
    let contact = contact_body(request)?;
    let stored = storage::contacts::insert_contact(&state.connect()?, &contact, ContactSource::Owner, Utc::now())?;
    Ok(Response::json(&stored).with_status_code(201))
}

fn edit_contact(state: &AppState, request: &Request, id: &ContactId) -> Handled {
    let contact = contact_body(request)?;
    let connection = state.connect()?;
    if !storage::contacts::update_contact(&connection, id, &contact, Utc::now())? {
        return Err(AppError::NotFound(format!("contact `{id}` not found")));
    }
    let stored = storage::contacts::get_contact(&connection, id)?
        .ok_or_else(|| AppError::NotFound(format!("contact `{id}` not found")))?;
    Ok(Response::json(&stored))
}

fn delete_contact(state: &AppState, id: &ContactId) -> Handled {
    if !storage::contacts::delete_contact(&state.connect()?, id)? {
        return Err(AppError::NotFound(format!("contact `{id}` not found")));
    }
    Ok(Response::empty_204())
}

fn delete_case(state: &AppState, id: &CaseId) -> Handled {
    state.engine.delete_case(&mut state.connect()?, id)?;
    Ok(Response::empty_204())
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

/// Body of `POST /human-requests/{id}/answer`: `text` for a question, `decision` (with
/// an optional `comment` and edited `args`) for an approval.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerBody {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    decision: Option<Decision>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    args: Option<Value>,
}

fn answer_human_request(state: &AppState, request: &Request, id: &HumanRequestId) -> Handled {
    let body: AnswerBody = json_body(request)?;
    let mut connection = state.connect()?;
    let answered = match (body.decision, body.text) {
        (Some(decision), None) => {
            if body.args.as_ref().is_some_and(|args| !args.is_object()) {
                return Err(AppError::BadRequest("`args` must be an object".to_owned()));
            }
            let comment = body.comment.as_deref().map(str::trim).filter(|comment| !comment.is_empty());
            let verdict = Verdict {
                decision,
                args: body.args.as_ref().filter(|_| decision == Decision::Approve),
                comment,
                via: "web",
                responder: None,
            };
            state.engine.decide_approval(&mut connection, id, verdict)?
        }
        (None, Some(text)) if !text.trim().is_empty() => {
            let answer = Answer {
                text: &text,
                via: "web",
                responder: None,
            };
            state.engine.answer_request(&mut connection, id, answer)?
        }
        _ => {
            return Err(AppError::BadRequest(
                "send `text` to answer a question, or `decision` (approve or reject) for an approval".to_owned(),
            ));
        }
    };
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
    let plugins = state.plugins.reload();
    let mut summary = prompts_summary(&prompts);
    if let Some(object) = summary.as_object_mut() {
        object.insert("plugin_errors".to_owned(), json!(plugins.errors()));
    }
    Response::json(&summary)
}

/// The owner's own prompt, added to every case (design §7.6).
fn user_prompt_json(state: &AppState) -> Handled {
    let (content, updated_at) = state.engine.user_prompt()?.unzip();
    Ok(Response::json(&json!({
        "content": content.unwrap_or_default(),
        "updated_at": updated_at,
        "max_chars": clankjob_engine::user_prompt::MAX_USER_PROMPT_CHARS,
    })))
}

/// Body of `PUT /user-prompt`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserPromptBody {
    content: String,
}

fn save_user_prompt(state: &AppState, request: &Request) -> Handled {
    let body: UserPromptBody = json_body(request)?;
    state.engine.set_user_prompt(&body.content)?;
    user_prompt_json(state)
}

/// An instance's status with how its channel has been doing, and whether it needs a look.
fn instance_json(state: &AppState, instance: &InstanceStatus) -> (Value, bool) {
    let activity = state.engine.channels().activity(&instance.name);
    let attention = instance.state == InstanceState::Error
        || instance.error.is_some()
        || !instance.problems.is_empty()
        || (instance.state == InstanceState::On && activity.failing());
    let mut value = json!(instance);
    if let Some(object) = value.as_object_mut() {
        object.insert("activity".to_owned(), json!(activity));
        object.insert("attention".to_owned(), Value::Bool(attention));
    }
    (value, attention)
}

/// Every plugin found, its instances, and whether anything needs a look (design §14.6).
fn plugins_summary(state: &AppState) -> Value {
    let mut attention = false;
    let plugins: Vec<Value> = state
        .plugins
        .current()
        .statuses()
        .iter()
        .map(|plugin| {
            attention |= plugin.error.is_some();
            let instances: Vec<Value> = plugin
                .instances
                .iter()
                .map(|instance| {
                    let (value, needs_look) = instance_json(state, instance);
                    attention |= needs_look;
                    value
                })
                .collect();
            let mut value = json!(plugin);
            if let Some(object) = value.as_object_mut() {
                object.insert("instances".to_owned(), Value::Array(instances));
            }
            value
        })
        .collect();
    let conflicts = state.plugins.conflicts();
    let paths = state.plugins.paths();
    json!({
        "plugins_dir": paths.dirs.last().map(|dir| dir.display().to_string()),
        "plugin_dirs": paths.dirs.iter().map(|dir| dir.display().to_string()).collect::<Vec<_>>(),
        "plugin_config_dir": paths.config_dir.as_ref().map(|dir| dir.display().to_string()),
        "attention": attention || !conflicts.is_empty(),
        "conflicts": conflicts,
        "plugins": plugins,
    })
}

/// Check an instance again, e.g. after fixing a permission in Discord.
fn test_instance(state: &AppState, name: &str) -> Handled {
    let registry = state.plugins.current();
    if let Some(tested) = registry.test(name) {
        return Ok(Response::json(&instance_json(state, &tested).0));
    }
    match registry.instance(name) {
        None => Err(AppError::NotFound(format!("plugin instance `{name}` not found"))),
        Some(instance) if instance.state == InstanceState::Off => Err(AppError::Conflict(format!(
            "`{name}` is turned off (enabled = false in its config.toml)"
        ))),
        Some(instance) => Err(AppError::Conflict(format!(
            "`{name}` did not load: {}",
            instance.error.unwrap_or_default()
        ))),
    }
}

fn route(state: &AppState, request: &Request) -> Handled {
    router!(request,
        (GET) (/api/v1/cases) => { list_cases(state, request) },
        (POST) (/api/v1/cases) => { create_case(state, request) },
        (GET) (/api/v1/cases/{id: String}) => { get_case(state, &CaseId::from_string(id)) },
        (PATCH) (/api/v1/cases/{id: String}) => { update_case(state, request, &CaseId::from_string(id)) },
        (DELETE) (/api/v1/cases/{id: String}) => { delete_case(state, &CaseId::from_string(id)) },
        (POST) (/api/v1/cases/{id: String}/instructions) => {
            change_instruction(state, Some(request), &CaseId::from_string(id), None)
        },
        (PUT) (/api/v1/cases/{id: String}/instructions/{instruction: String}) => {
            change_instruction(state, Some(request), &CaseId::from_string(id), Some(&InstructionId::from_string(instruction)))
        },
        (DELETE) (/api/v1/cases/{id: String}/instructions/{instruction: String}) => {
            change_instruction(state, None, &CaseId::from_string(id), Some(&InstructionId::from_string(instruction)))
        },
        (POST) (/api/v1/cases/{id: String}/files) => { upload_file(state, request, &CaseId::from_string(id)) },
        (GET) (/api/v1/cases/{id: String}/files/{file: String}) => {
            get_file(state, &CaseId::from_string(id), &FileId::from_string(file))
        },
        (GET) (/api/v1/cases/{id: String}/files/{file: String}/content) => {
            file_content(state, &CaseId::from_string(id), &FileId::from_string(file))
        },
        (GET) (/api/v1/cases/{id: String}/events) => { list_events(state, request, &CaseId::from_string(id)) },
        (POST) (/api/v1/cases/{id: String}/messages) => { post_message(state, request, &CaseId::from_string(id)) },
        (POST) (/api/v1/cases/{id: String}/wake) => { wake_case(state, &CaseId::from_string(id)) },
        (POST) (/api/v1/cases/{id: String}/cancel) => { cancel_case(state, &CaseId::from_string(id)) },
        (GET) (/api/v1/contacts) => { list_contacts(state) },
        (POST) (/api/v1/contacts) => { add_contact(state, request) },
        (PUT) (/api/v1/contacts/{id: String}) => { edit_contact(state, request, &ContactId::from_string(id)) },
        (DELETE) (/api/v1/contacts/{id: String}) => { delete_contact(state, &ContactId::from_string(id)) },
        (GET) (/api/v1/human-requests) => { list_human_requests(state, request) },
        (POST) (/api/v1/human-requests/{id: String}/answer) => {
            answer_human_request(state, request, &HumanRequestId::from_string(id))
        },
        (GET) (/api/v1/llms) => {
            Ok(Response::json(&json!({ "default_llm": state.defaults.llm, "llms": state.catalog.choices() })))
        },
        (GET) (/api/v1/channels) => { Ok(Response::json(&json!({ "channels": state.engine.channels().list() }))) },
        (GET) (/api/v1/prompts) => { Ok(Response::json(&prompts_summary(&state.engine.prompts()))) },
        (GET) (/api/v1/prompts/profiles/{name: String}) => { get_prompt(state, &format!("profiles/{name}")) },
        (GET) (/api/v1/prompts/{name: String}) => { get_prompt(state, &name) },
        (GET) (/api/v1/user-prompt) => { user_prompt_json(state) },
        (PUT) (/api/v1/user-prompt) => { save_user_prompt(state, request) },
        (GET) (/api/v1/plugins) => { Ok(Response::json(&plugins_summary(state))) },
        (POST) (/api/v1/plugins/reload) => {
            state.plugins.reload();
            Ok(Response::json(&plugins_summary(state)))
        },
        (POST) (/api/v1/plugin-instances/{name: String}/test) => { test_instance(state, &name) },
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
    let declared = request
        .header("Content-Length")
        .and_then(|length| length.trim().parse::<u64>().ok());
    let is_upload = request.method() == "POST" && request.url().ends_with("/files");
    let limit = if is_upload { MAX_UPLOAD_BYTES } else { MAX_BODY_BYTES };
    if declared.is_some_and(|length| length > limit) {
        return AppError::PayloadTooLarge.into_response();
    }
    route(state, request).unwrap_or_else(AppError::into_response)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use clankjob_core::llm::{CompletionRequest, CompletionResponse, LlmError, LlmProvider};
    use clankjob_engine::EngineSettings;
    use clankjob_storage::Db;
    use tempfile::TempDir;

    use super::*;
    use crate::catalog::CatalogLlm;

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
        dir: TempDir,
    }

    impl TestApi {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = Db::new(dir.path().join("test.db"));
            db.migrate().unwrap();
            let providers = HashMap::from([
                ("default".to_owned(), Arc::new(UnusedProvider) as Arc<dyn LlmProvider>),
                ("other".to_owned(), Arc::new(UnusedProvider) as Arc<dyn LlmProvider>),
            ]);
            let engine = Engine::new(
                db,
                providers,
                None,
                dir.path().join("files"),
                clankjob_engine::channels::Channels::default(),
                EngineSettings {
                    user_prompt_path: Some(dir.path().join("user_prompt.md")),
                    ..EngineSettings::default()
                },
            );
            let defaults = CaseDefaults {
                llm: "default".to_owned(),
                profile: None,
                budgets: Budgets::default(),
            };
            let catalog = Catalog::new(vec![CatalogLlm {
                name: "default".to_owned(),
                model: "big-model".to_owned(),
                suggested: vec!["cheap-model".to_owned(), "big-model".to_owned()],
                provider: None,
            }]);
            let paths = clankjob_plugin_host::PluginPaths {
                secrets_dir: dir.path().to_path_buf(),
                ..Default::default()
            };
            let plugins = Arc::new(PluginManager::new(paths, None, engine.clone()));
            let state = AppState::new(
                engine,
                Some(vec![SecretString::from("token".to_owned())]),
                defaults,
                Arc::new(catalog),
                plugins,
            );
            Self { state, dir }
        }

        fn without_token() -> Self {
            let mut api = Self::new();
            api.state.tokens = None;
            api
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
    fn without_a_required_token_any_request_is_served_and_health_says_so() {
        let api = TestApi::without_token();

        let response = handle(
            &api.state,
            &Request::fake_http("GET", "/api/v1/cases", vec![], Vec::new()),
        );
        let health = handle(&api.state, &Request::fake_http("GET", "/healthz", vec![], Vec::new()));

        assert_eq!(response.status_code, 200);
        let (mut reader, _) = health.data.into_reader_and_size();
        let mut text = String::new();
        reader.read_to_string(&mut text).unwrap();
        let health = serde_json::from_str::<Value>(&text).unwrap();
        assert_eq!(health["token_required"], json!(false));
        assert_eq!(health["version"], json!(env!("CLANKJOB_VERSION")));
        assert_eq!(health["commit"], json!(env!("CLANKJOB_COMMIT")));
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
    fn a_case_created_with_only_a_title_waits_for_the_owner() {
        let api = TestApi::new();

        let (status, case) = api.call("POST", "/api/v1/cases", Some(json!({"title": "Panel upgrade"})));

        assert_eq!(status, 201);
        assert_eq!(
            (case["state"].as_str(), case["goal"].as_str()),
            (Some("waiting_for_human"), Some(""))
        );
    }

    #[test]
    fn a_case_can_be_renamed_and_bad_titles_are_refused() {
        let api = TestApi::new();
        let id = api.create_case();
        let path = format!("/api/v1/cases/{id}");

        let (status, detail) = api.call("PATCH", &path, Some(json!({"title": "  Panel quote, Laval  "})));
        let empty = api.call("PATCH", &path, Some(json!({"title": " "}))).0;
        let long = api.call("PATCH", &path, Some(json!({"title": "x".repeat(201)}))).0;
        let nothing = api.call("PATCH", &path, Some(json!({}))).0;
        let unknown = api.call("PATCH", "/api/v1/cases/nope", Some(json!({"title": "t"}))).0;

        assert_eq!(status, 200);
        assert_eq!(detail["case"]["title"], "Panel quote, Laval");
        assert_eq!((empty, long, nothing, unknown), (400, 400, 400, 404));
    }

    #[test]
    fn a_cases_model_can_be_changed_while_it_works() {
        let api = TestApi::new();
        let (_, case) = api.call("POST", "/api/v1/cases", Some(json!({"title": "t", "model": "a/one"})));
        let id = case["id"].as_str().unwrap().to_owned();
        let path = format!("/api/v1/cases/{id}");
        let change = |body: Value| api.call("PATCH", &path, Some(body));
        let llm_model = |detail: &Value| {
            (
                detail["case"]["llm"].as_str().map(str::to_owned),
                detail["case"]["model"].as_str().map(str::to_owned),
            )
        };

        let (status, model) = change(json!({"model": " b/two "}));
        let (_, other_llm) = change(json!({"llm": "other"}));
        change(json!({"model": "c/three"}));
        let (_, default) = change(json!({"model": null}));
        change(json!({"model": "c/three"}));
        let (_, blank) = change(json!({"model": " "}));
        let (_, both) = change(json!({"llm": "default", "model": "d/four"}));
        let unknown_llm = change(json!({"llm": "nope", "title": "renamed"})).0;
        let empty_llm = change(json!({"llm": " "})).0;
        let long_model = change(json!({"model": "m".repeat(201)})).0;
        let unknown_channel = change(json!({"human_channels": ["discord_joe"], "title": "renamed"})).0;
        let (web_only, web) = change(json!({"human_channels": []}));
        let (_, after) = api.call("GET", &path, None);

        assert_eq!(status, 200);
        assert_eq!(
            llm_model(&model),
            (Some("default".to_owned()), Some("b/two".to_owned()))
        );
        assert_eq!(
            llm_model(&other_llm),
            (Some("other".to_owned()), None),
            "another LLM starts from its default model"
        );
        assert_eq!(llm_model(&default), (Some("other".to_owned()), None));
        assert_eq!(llm_model(&blank), (Some("other".to_owned()), None));
        assert_eq!(
            llm_model(&both),
            (Some("default".to_owned()), Some("d/four".to_owned()))
        );
        assert_eq!(
            (unknown_llm, empty_llm, long_model, unknown_channel),
            (400, 400, 400, 400)
        );
        assert_eq!((web_only, web["case"]["human_channels"].clone()), (200, json!([])));
        assert_eq!(after["case"]["title"], "t", "a refused change changes nothing else");
        assert_eq!(
            llm_model(&after),
            (Some("default".to_owned()), Some("d/four".to_owned()))
        );
    }

    #[test]
    fn contacts_are_added_edited_listed_and_deleted_by_the_owner() {
        let api = TestApi::new();

        let (created, robin) = api.call(
            "POST",
            "/api/v1/contacts",
            Some(json!({"name": " Robin ", "email": "Robin@Sparky.CA", "note": "electrician"})),
        );
        let id = robin["id"].as_str().unwrap().to_owned();
        let (edited, trusted) = api.call(
            "PUT",
            &format!("/api/v1/contacts/{id}"),
            Some(json!({"name": "Robin Tremblay", "email": "robin@sparky.ca", "trusted": true})),
        );
        let (_, list) = api.call("GET", "/api/v1/contacts", None);
        let bad_email = api
            .call("POST", "/api/v1/contacts", Some(json!({"name": "X", "email": "nope"})))
            .0;
        let no_name = api.call("POST", "/api/v1/contacts", Some(json!({"name": " "}))).0;
        let deleted = api.call("DELETE", &format!("/api/v1/contacts/{id}"), None).0;
        let gone = api.call("PUT", &format!("/api/v1/contacts/{id}"), Some(json!({"name": "R"}))).0;

        assert_eq!(
            (created, edited, bad_email, no_name, deleted, gone),
            (201, 200, 400, 400, 204, 404)
        );
        assert_eq!(
            (
                robin["name"].as_str(),
                robin["email"].as_str(),
                robin["added_by"].as_str(),
                robin["trusted"].as_bool()
            ),
            (Some("Robin"), Some("robin@sparky.ca"), Some("owner"), Some(false))
        );
        assert_eq!(
            (trusted["name"].as_str(), trusted["trusted"].as_bool()),
            (Some("Robin Tremblay"), Some(true))
        );
        assert_eq!(trusted["note"], Value::Null, "a replaced contact loses fields left out");
        assert_eq!(list["contacts"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn a_cases_approvals_are_set_at_creation_and_changed_later() {
        let api = TestApi::new();

        let (_, case) = api.call(
            "POST",
            "/api/v1/cases",
            Some(json!({"title": "t", "approvals": "never"})),
        );
        let id = case["id"].as_str().unwrap();
        let (status, detail) = api.call(
            "PATCH",
            &format!("/api/v1/cases/{id}"),
            Some(json!({"approvals": "always"})),
        );
        let unknown = api
            .call(
                "PATCH",
                &format!("/api/v1/cases/{id}"),
                Some(json!({"approvals": "sometimes"})),
            )
            .0;
        let (_, default) = api.call("POST", "/api/v1/cases", Some(json!({"title": "u"})));

        assert_eq!(case["approvals"], "never");
        assert_eq!((status, detail["case"]["approvals"].as_str()), (200, Some("always")));
        assert_eq!(unknown, 400);
        assert_eq!(default["approvals"], "default");
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
        assert_eq!(list["prompts"].as_array().unwrap().len(), 9);
        assert_eq!(prompt["source"], "builtin");
        assert_eq!(api.call("GET", "/api/v1/prompts/profiles/none", None).0, 404);
    }

    #[test]
    fn llms_list_the_default_model_first_without_duplicates() {
        let api = TestApi::new();

        let (status, body) = api.call("GET", "/api/v1/llms", None);

        assert_eq!(status, 200);
        assert_eq!(body["default_llm"], "default");
        assert_eq!(body["llms"][0]["model"], "big-model");
        assert_eq!(
            body["llms"][0]["models"],
            json!([{"id": "big-model"}, {"id": "cheap-model"}])
        );
    }

    #[test]
    fn case_can_be_started_with_a_chosen_model() {
        let api = TestApi::new();

        let (status, case) = api.call(
            "POST",
            "/api/v1/cases",
            Some(json!({"title": "t", "goal": "g", "llm": "default", "model": "cheap-model"})),
        );

        assert_eq!(status, 201);
        assert_eq!(case["model"], "cheap-model");
    }

    fn upload(api: &TestApi, url: &str, bytes: &[u8]) -> (u16, Value) {
        let headers = vec![("Authorization".to_owned(), "Bearer token".to_owned())];
        let response = handle(&api.state, &Request::fake_http("POST", url, headers, bytes.to_vec()));
        let (mut reader, _) = response.data.into_reader_and_size();
        let mut text = String::new();
        reader.read_to_string(&mut text).unwrap();
        (response.status_code, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    #[test]
    fn a_deleted_case_is_gone_with_its_rows_and_files_but_a_running_one_is_kept() {
        // Arrange
        let api = TestApi::new();
        let id = api.create_case();
        let (_, file) = upload(&api, &format!("/api/v1/cases/{id}/files?name=quote.txt"), b"$450");
        let bytes = api.dir.path().join("files").join(file["id"].as_str().unwrap());
        api.call(
            "POST",
            &format!("/api/v1/cases/{id}/messages"),
            Some(json!({"text": "hi"})),
        );
        let running = api.create_case();
        let connection = api.state.connect().unwrap();
        storage::cases::update_state(
            &connection,
            &CaseId::from_string(running.clone()),
            CaseState::Running,
            Utc::now(),
        )
        .unwrap();

        // Act
        let (deleted, _) = api.call("DELETE", &format!("/api/v1/cases/{id}"), None);
        let (after, _) = api.call("GET", &format!("/api/v1/cases/{id}"), None);
        let (again, _) = api.call("DELETE", &format!("/api/v1/cases/{id}"), None);
        let (refused, _) = api.call("DELETE", &format!("/api/v1/cases/{running}"), None);

        // Assert
        assert_eq!((deleted, after, again, refused), (204, 404, 404, 409));
        assert!(!bytes.exists(), "the file's bytes are removed too");
        let events: i64 = connection
            .query_row("SELECT COUNT(*) FROM events WHERE case_id = ?1", [id.as_str()], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(events, 0);
        assert_eq!(api.call("GET", &format!("/api/v1/cases/{running}"), None).0, 200);
    }

    #[test]
    fn files_are_uploaded_listed_read_and_served() {
        // Arrange
        let api = TestApi::new();
        let id = api.create_case();

        // Act
        let (status, file) = upload(
            &api,
            &format!("/api/v1/cases/{id}/files?name=tone.md"),
            b"# Tone\nBe polite.",
        );
        let (image_status, image) = upload(
            &api,
            &format!("/api/v1/cases/{id}/files?name=p.png"),
            b"\x89PNG\r\n\x1a\n",
        );
        let (_, detail) = api.call("GET", &format!("/api/v1/cases/{id}"), None);
        let file_id = file["id"].as_str().unwrap();
        let (_, full) = api.call("GET", &format!("/api/v1/cases/{id}/files/{file_id}"), None);
        let request = Request::fake_http(
            "GET",
            format!("/api/v1/cases/{id}/files/{}/content", image["id"].as_str().unwrap()),
            vec![("Authorization".to_owned(), "Bearer token".to_owned())],
            Vec::new(),
        );
        let content = handle(&api.state, &request);

        // Assert
        assert_eq!((status, image_status), (201, 201));
        assert_eq!(file["kind"], "text");
        assert_eq!(image["media_type"], "image/png");
        assert_eq!(detail["files"].as_array().unwrap().len(), 2);
        assert!(detail["files"][0].get("text").is_none());
        assert_eq!(full["text"], "# Tone\nBe polite.");
        assert_eq!(content.status_code, 200);
        assert!(
            content
                .headers
                .iter()
                .any(|(name, value)| name == "Content-Type" && value == "image/png")
        );
        assert!(
            content
                .headers
                .iter()
                .any(|(name, value)| name == "Content-Disposition" && value.starts_with("inline"))
        );
        // The upload woke the case, like a message.
        let (_, events) = api.call("GET", &format!("/api/v1/cases/{id}/events"), None);
        assert!(
            events["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["payload"]["reason"] == "file_added")
        );
    }

    #[test]
    fn channels_are_listed_and_unknown_ones_rejected() {
        // Arrange
        let api = TestApi::new();

        // Act
        let (listed, channels) = api.call("GET", "/api/v1/channels", None);
        let body = json!({"title": "t", "goal": "g", "human_channels": ["discord_joe"]});
        let (unknown, error) = api.call("POST", "/api/v1/cases", Some(body));
        let (created, case) = api.call("POST", "/api/v1/cases", Some(json!({"title": "t", "goal": "g"})));
        let (_, detail) = api.call("GET", &format!("/api/v1/cases/{}", case["id"].as_str().unwrap()), None);

        // Assert
        assert_eq!(detail["cost"]["usd"], Value::Null, "the test catalog has no prices");
        assert_eq!(detail["cost"]["model"], "big-model");
        assert_eq!((listed, channels), (200, json!({ "channels": [] })));
        assert_eq!(unknown, 400);
        assert_eq!(error["error"]["message"], "unknown human channel `discord_joe`");
        assert_eq!((created, case["human_channels"].clone()), (201, json!([])));
    }

    #[test]
    fn approvals_take_a_decision_once() {
        // Arrange
        let api = TestApi::new();
        let (_, case) = api.call("POST", "/api/v1/cases", Some(json!({"title": "t", "goal": "g"})));
        let case_id = CaseId::from_string(case["id"].as_str().unwrap());
        let id = HumanRequestId::generate();
        let args = json!({"to": "bob@x.ca", "body": "Hi"});
        let connection = api.state.connect().unwrap();
        storage::human::insert_approval(
            &connection,
            &id,
            &case_id,
            "Email bob@x.ca",
            "send_email",
            &args,
            Utc::now(),
        )
        .unwrap();
        let url = format!("/api/v1/human-requests/{id}/answer");

        // Act
        let (as_text, _) = api.call("POST", &url, Some(json!({"text": "yes"})));
        let (listed, open) = api.call("GET", "/api/v1/human-requests", None);
        let edited = json!({"to": "bob@x.ca", "body": "Hello Bob"});
        let (approved, request) = api.call(
            "POST",
            &url,
            Some(json!({"decision": "approve", "args": edited, "comment": "ok"})),
        );
        let (again, _) = api.call("POST", &url, Some(json!({"decision": "reject"})));

        // Assert
        assert_eq!(as_text, 400);
        assert_eq!(
            (listed, open["human_requests"][0]["kind"].clone()),
            (200, json!("approval"))
        );
        assert_eq!(open["human_requests"][0]["args"], args);
        assert_eq!(approved, 200);
        assert_eq!(
            (request["decision"].clone(), request["execution"].clone()),
            (json!("approve"), json!("pending"))
        );
        assert_eq!(request["args"], edited);
        assert_eq!(again, 409);
    }

    #[test]
    fn the_user_prompt_is_read_saved_and_limited() {
        // Arrange
        let api = TestApi::new();

        // Act
        let (_, empty) = api.call("GET", "/api/v1/user-prompt", None);
        let (saved, prompt) = api.call(
            "PUT",
            "/api/v1/user-prompt",
            Some(json!({"content": "Sign emails as Brad."})),
        );
        let too_long = "x".repeat(clankjob_engine::user_prompt::MAX_USER_PROMPT_CHARS + 1);
        let (rejected, _) = api.call("PUT", "/api/v1/user-prompt", Some(json!({"content": too_long})));
        let (_, after) = api.call("GET", "/api/v1/user-prompt", None);

        // Assert
        assert_eq!(
            (empty["content"].clone(), empty["updated_at"].clone()),
            (json!(""), Value::Null)
        );
        assert_eq!((saved, prompt["content"].clone()), (200, json!("Sign emails as Brad.")));
        assert!(prompt["updated_at"].is_string());
        assert_eq!(rejected, 400);
        assert_eq!(
            after["content"], "Sign emails as Brad.",
            "a rejected save changes nothing"
        );
    }

    #[test]
    fn instructions_are_set_at_creation_and_edited_later() {
        // Arrange
        let api = TestApi::new();
        let body = json!({"title": "t", "goal": "g", "instructions": [{"name": " tone.md ", "content": "Be polite."}]});
        let (created, case) = api.call("POST", "/api/v1/cases", Some(body));
        let id = case["id"].as_str().unwrap();
        let (_, detail) = api.call("GET", &format!("/api/v1/cases/{id}"), None);
        let tone = detail["instructions"][0]["id"].as_str().unwrap().to_owned();

        // Act
        let base = format!("/api/v1/cases/{id}/instructions");
        let (added, budget) = api.call(
            "POST",
            &base,
            Some(json!({"name": "budget.md", "content": "Max $1,500."})),
        );
        let (edited, _) = api.call(
            "PUT",
            &format!("{base}/{tone}"),
            Some(json!({"name": "tone.md", "content": "Be firm."})),
        );
        let (removed, _) = api.call("DELETE", &format!("{base}/{}", budget["id"].as_str().unwrap()), None);
        let (_, after) = api.call("GET", &format!("/api/v1/cases/{id}"), None);

        // Assert
        assert_eq!((created, added, edited, removed), (201, 201, 200, 200));
        assert_eq!(detail["instructions"][0]["name"], "tone.md");
        assert_eq!(after["instructions"].as_array().unwrap().len(), 1);
        assert_eq!(after["instructions"][0]["content"], "Be firm.");
        assert_eq!(api.call("DELETE", &format!("{base}/nope"), None).0, 404);
        assert_eq!(
            api.call("POST", &base, Some(json!({"name": "x.md", "content": " "}))).0,
            400
        );
    }

    #[test]
    fn bad_uploads_are_rejected() {
        let api = TestApi::new();
        let id = api.create_case();
        let oversized_headers = vec![
            ("Authorization".to_owned(), "Bearer token".to_owned()),
            ("Content-Length".to_owned(), (MAX_UPLOAD_BYTES + 1).to_string()),
        ];

        let (binary, error) = upload(
            &api,
            &format!("/api/v1/cases/{id}/files?name=a.exe"),
            b"MZ\x90\x00\xff\xfe",
        );
        let (no_name, _) = upload(&api, &format!("/api/v1/cases/{id}/files"), b"text");
        let (no_case, _) = upload(&api, "/api/v1/cases/nope/files?name=a.txt", b"text");
        let oversized = handle(
            &api.state,
            &Request::fake_http(
                "POST",
                format!("/api/v1/cases/{id}/files?name=big.txt"),
                oversized_headers,
                Vec::new(),
            ),
        );
        let big_json = handle(
            &api.state,
            &Request::fake_http(
                "POST",
                "/api/v1/cases",
                vec![
                    ("Authorization".to_owned(), "Bearer token".to_owned()),
                    ("Content-Length".to_owned(), (MAX_BODY_BYTES + 1).to_string()),
                ],
                Vec::new(),
            ),
        );

        assert_eq!(binary, 400);
        assert!(error["error"]["message"].as_str().unwrap().contains("not a supported file"));
        assert_eq!((no_name, no_case), (400, 404));
        assert_eq!((oversized.status_code, big_json.status_code), (413, 413));
    }

    #[test]
    fn constant_time_eq_compares_content_and_length() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
