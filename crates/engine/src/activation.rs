//! The activation loop (design §5): run the LLM ↔ tool loop for one claimed case until it
//! sleeps, asks a human, finishes, or must be retried later.
//!
//! Every step is committed before the next one starts, and the loop re-reads the event log
//! at each step. After a crash, the next activation therefore resumes exactly where this
//! one stopped: tool calls the LLM already requested are run without asking it again.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::thread;

use chrono::Utc;
use clankjob_core::case::{ApprovalPolicy, Case, CaseState};
use clankjob_core::event::{Event, EventBody, ToolResult, WakeReason};
use clankjob_core::file::{CaseFile, FileKind};
use clankjob_core::human::Execution;
use clankjob_core::ids::{ActivationId, CaseId, FileId, HumanRequestId, WaitConditionId};
use clankjob_core::llm::{CompletionRequest, CompletionResponse, LlmError, LlmProvider, TokenUsage, ToolCall};
use clankjob_core::tool::{CaseFileRef, Guide, ToolContext, ToolOutput};
use clankjob_core::wait::{HUMAN_INPUT_KIND, WaitCondition, WaitConditionSpec, WaitStatus};
use clankjob_storage::{self as storage, Connection, begin_write, commit};
use serde_json::{Value, json};

use crate::context::{build_messages, pending_tool_calls};
use crate::files::{FileStore, chunk, find, views};
use crate::plugin_tools::PluginTools;
use crate::prompts::{
    CASE_HEADER, CaseView, FILES, GUIDES, INSTRUCTIONS, PLUGINS, PromptContext, PromptSet, SYSTEM, USER_PROMPT,
};
use crate::tools::{
    AskHumanArgs, CORE_TOOL_NAMES, CoreTool, DEFAULT_READ_CHARS, MAX_READ_CHARS, ReadFileArgs, add, core_tool_specs,
    describe_plugin_conditions, load_plugin_spec, schedule,
};
use crate::transitions::{ClaimedCase, change_state, load_case};
use crate::{Result, Shared, later};

/// Whether the activation goes on after a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

/// What running one tool call produced.
#[derive(Debug, Clone, PartialEq)]
struct ToolExecution {
    content: Value,
    is_error: bool,
    /// Set when the call suspends or finishes the case.
    end_state: Option<CaseState>,
}

impl ToolExecution {
    fn ok(content: Value) -> Self {
        Self {
            content,
            is_error: false,
            end_state: None,
        }
    }

    fn error(message: String) -> Self {
        Self {
            content: json!({ "error": Value::String(message) }),
            is_error: true,
            end_state: None,
        }
    }

    fn ending(content: Value, end_state: CaseState) -> Self {
        Self {
            content,
            is_error: false,
            end_state: Some(end_state),
        }
    }
}

/// Validate conditions and store them, or return a message for the LLM.
///
/// Nothing is stored unless every condition is valid.
fn register_waits(
    connection: &Connection,
    case: &Case,
    specs: &[WaitConditionSpec],
    plugins: &PluginTools,
) -> Result<std::result::Result<Vec<Value>, String>> {
    let now = Utc::now();
    let schedules: std::result::Result<Vec<_>, String> =
        specs.iter().map(|spec| schedule(spec, now, plugins)).collect();
    let schedules = match schedules {
        Ok(schedules) => schedules,
        Err(message) => return Ok(Err(message)),
    };
    let mut registered = Vec::with_capacity(specs.len());
    for (spec, schedule) in specs.iter().zip(schedules) {
        let condition = WaitCondition {
            id: WaitConditionId::generate(),
            case_id: case.id.clone(),
            kind: spec.kind.clone(),
            params: spec.params.clone(),
            next_check_at: schedule.next_check_at,
            deadline_at: schedule.deadline_at,
            status: WaitStatus::Active,
            created_at: now,
        };
        storage::waits::insert_wait(connection, &condition)?;
        if let Some(every) = schedule.check_every {
            let millis = i64::try_from(every.as_millis()).unwrap_or(i64::MAX);
            storage::waits::set_check_interval(connection, &condition.id, millis)?;
        }
        registered.push(json!({
            "id": condition.id,
            "kind": condition.kind,
            "next_check_at": condition.next_check_at,
            "deadline_at": condition.deadline_at,
        }));
    }
    Ok(Ok(registered))
}

/// Open a human request and wait for it and for any extra conditions.
fn ask_human(
    connection: &Connection,
    case: &Case,
    args: &AskHumanArgs,
    plugins: &PluginTools,
) -> Result<ToolExecution> {
    let now = Utc::now();
    let deadline_at = match args.timeout.map(|timeout| add(now, timeout)).transpose() {
        Ok(deadline_at) => deadline_at,
        Err(message) => return Ok(ToolExecution::error(message)),
    };
    let also = match register_waits(connection, case, &args.also_wait_for, plugins)? {
        Ok(also) => also,
        Err(message) => return Ok(ToolExecution::error(message)),
    };
    let request_id = HumanRequestId::generate();
    storage::human::insert_request(connection, &request_id, &case.id, &args.question, now)?;
    crate::channels::queue_question(connection, case, &request_id, &args.question, now)?;
    let human_input = WaitCondition {
        id: WaitConditionId::generate(),
        case_id: case.id.clone(),
        kind: HUMAN_INPUT_KIND.to_owned(),
        params: json!({ "request_id": request_id }),
        next_check_at: None,
        deadline_at,
        status: WaitStatus::Active,
        created_at: now,
    };
    storage::waits::insert_wait(connection, &human_input)?;
    Ok(ToolExecution::ending(
        json!({ "status": "waiting_for_human", "request_id": request_id, "also_waiting_for": also }),
        CaseState::WaitingForHuman,
    ))
}

/// What happened to a call before its transaction: a plugin tool ran, needs approval, or
/// the call is for a core tool.
enum PluginCall {
    Core,
    Ran(std::result::Result<ToolOutput, String>),
    NeedsApproval(String),
    /// A tool that normally needs approval ran without it, for this reason.
    RanWithoutApproval(std::result::Result<ToolOutput, String>, String),
}

/// Open an approval for a plugin tool call and suspend the case until it is decided
/// (design §9.7). Nothing runs yet.
fn request_approval(connection: &Connection, case: &Case, call: &ToolCall, summary: &str) -> Result<ToolExecution> {
    let now = Utc::now();
    let request_id = HumanRequestId::generate();
    storage::human::insert_approval(
        connection,
        &request_id,
        &case.id,
        summary,
        &call.name,
        &call.arguments,
        now,
    )?;
    crate::channels::queue_approval(connection, case, &request_id, summary, &call.arguments, now)?;
    let human_input = WaitCondition {
        id: WaitConditionId::generate(),
        case_id: case.id.clone(),
        kind: HUMAN_INPUT_KIND.to_owned(),
        params: json!({ "request_id": request_id }),
        next_check_at: None,
        deadline_at: None,
        status: WaitStatus::Active,
        created_at: now,
    };
    storage::waits::insert_wait(connection, &human_input)?;
    Ok(ToolExecution::ending(
        json!({
            "status": "waiting_for_approval",
            "request_id": request_id,
            "summary": summary,
            "note": "Nothing was done yet. The owner must approve this call; when the case wakes you will be told whether it ran, and its result.",
        }),
        CaseState::WaitingForHuman,
    ))
}

/// What the file and guide tools need to know.
struct FileEnv<'a> {
    files: &'a [CaseFile],
    vision: bool,
    guides: &'a [Guide],
    plugins: &'a PluginTools,
}

/// `read_guide`: a plugin's instructions for a kind of task.
fn read_guide(guides: &[Guide], wanted: &str) -> ToolExecution {
    let wanted = wanted.trim();
    let found = guides
        .iter()
        .find(|guide| guide.name == wanted)
        .or_else(|| guides.iter().find(|guide| guide.name.eq_ignore_ascii_case(wanted)));
    if let Some(guide) = found {
        return ToolExecution::ok(json!({ "guide": guide.name, "plugin": guide.plugin, "content": guide.content }));
    }
    let names: Vec<&str> = guides.iter().map(|guide| guide.name.as_str()).collect();
    ToolExecution::error(format!("no guide named `{wanted}`; available: {}", names.join(", ")))
}

/// `load_plugin`: offer a plugin's tools from the next turn on. The case's event log keeps
/// the result, which is how later turns know the plugin is loaded.
fn load_plugin(plugins: &PluginTools, wanted: &str) -> ToolExecution {
    let wanted = wanted.trim();
    let index = plugins.index(&BTreeSet::new());
    let found = index
        .iter()
        .find(|plugin| plugin.id == wanted)
        .or_else(|| index.iter().find(|plugin| plugin.id.eq_ignore_ascii_case(wanted)));
    if let Some(plugin) = found {
        return ToolExecution::ok(json!({
            "plugin": plugin.id,
            "tools": plugin.tools,
            "conditions": plugin.conditions,
            "note": "Its tools are offered to you from your next turn on.",
        }));
    }
    let ids: Vec<&str> = index.iter().map(|plugin| plugin.id.as_str()).collect();
    ToolExecution::error(format!("no plugin named `{wanted}`; available: {}", ids.join(", ")))
}

/// A name for a tool's output file that no file of the case has yet.
fn unique_name(files: &[CaseFile], wanted: &str) -> String {
    if !files.iter().any(|file| file.name == wanted) {
        return wanted.to_owned();
    }
    let (stem, extension) = wanted
        .rsplit_once('.')
        .map_or((wanted, ""), |(stem, extension)| (stem, extension));
    (2_u32..10_000)
        .map(|number| {
            if extension.is_empty() {
                format!("{stem} ({number})")
            } else {
                format!("{stem} ({number}).{extension}")
            }
        })
        .find(|candidate| !files.iter().any(|file| &file.name == candidate))
        .unwrap_or_else(|| wanted.to_owned())
}

/// Turn a plugin tool's output into a tool result, storing file output as a case file.
///
/// # Returns
///
/// The result, and the id of a file whose bytes were written, to remove if the
/// transaction does not commit
fn plugin_result(
    connection: &Connection,
    case: &Case,
    store: &FileStore,
    files: &[CaseFile],
    output: std::result::Result<ToolOutput, String>,
) -> Result<(ToolExecution, Option<FileId>)> {
    let (name, content, summary) = match output {
        Ok(ToolOutput::Json(value)) => return Ok((ToolExecution::ok(value), None)),
        Err(message) => return Ok((ToolExecution::error(message), None)),
        Ok(ToolOutput::File { name, content, summary }) => (name, content, summary),
    };
    let file = match crate::prepare_file(connection, &case.id, &unique_name(files, &name), &content) {
        Ok(file) => file,
        Err(crate::EngineError::InvalidFile(message)) => {
            return Ok((
                ToolExecution::error(format!("the output could not be stored: {message}")),
                None,
            ));
        }
        Err(error) => return Err(error),
    };
    store.write(&file.id, &content)?;
    if let Err(error) = storage::files::insert_file(connection, &file) {
        store.remove(&file.id);
        return Err(error.into());
    }
    let mut result = json!({
        "stored_as": file.name,
        "chars": file.text_chars,
        "next": format!("Read it with `read_file` (file `{}`), in parts if it is long.", file.name),
    });
    if let (Some(object), Value::Object(extra)) = (result.as_object_mut(), summary) {
        object.extend(extra);
    }
    Ok((ToolExecution::ok(result), Some(file.id)))
}

/// `read_file`: a chunk of a text file or PDF.
fn read_file(env: &FileEnv<'_>, args: &ReadFileArgs) -> ToolExecution {
    let file = match find(env.files, &args.file) {
        Ok(file) => file,
        Err(message) => return ToolExecution::error(message),
    };
    let Some(text) = &file.text else {
        let why = if file.kind == FileKind::Image {
            "is an image: use `view_image`"
        } else {
            "has no text layer (probably a scan)"
        };
        return ToolExecution::error(format!("`{}` {why}", file.name));
    };
    let max_chars = args.max_chars.unwrap_or(DEFAULT_READ_CHARS).clamp(1, MAX_READ_CHARS);
    let (part, next_offset) = chunk(text, args.offset, max_chars);
    ToolExecution::ok(json!({
        "file": file.name,
        "offset": args.offset,
        "text": part,
        "next_offset": next_offset,
        "total_chars": file.text_chars,
    }))
}

/// `view_image`: the image itself is attached when the conversation is rebuilt.
fn view_image(env: &FileEnv<'_>, wanted: &str) -> ToolExecution {
    if !env.vision {
        return ToolExecution::error("the current model cannot see images".to_owned());
    }
    match find(env.files, wanted) {
        Ok(file) if file.kind == FileKind::Image => {
            ToolExecution::ok(json!({ "status": "shown", "file": file.name, "image_file_id": file.id }))
        }
        Ok(file) => ToolExecution::error(format!("`{}` is not an image: use `read_file`", file.name)),
        Err(message) => ToolExecution::error(message),
    }
}

/// Run one core tool call inside the caller's transaction.
fn execute(connection: &Connection, case: &Case, call: &ToolCall, env: &FileEnv<'_>) -> Result<ToolExecution> {
    let tool = match CoreTool::parse(call) {
        Ok(tool) => tool,
        Err(message) => return Ok(ToolExecution::error(message)),
    };
    let now = Utc::now();
    Ok(match tool {
        CoreTool::Sleep(args) if args.conditions.is_empty() => {
            ToolExecution::error("`sleep` needs at least one condition".to_owned())
        }
        CoreTool::Sleep(args) => match register_waits(connection, case, &args.conditions, env.plugins)? {
            Ok(conditions) => ToolExecution::ending(
                json!({ "status": "sleeping", "conditions": conditions, "fresh": args.fresh }),
                CaseState::Sleeping,
            ),
            Err(message) => ToolExecution::error(message),
        },
        CoreTool::AskHuman(args) => ask_human(connection, case, &args, env.plugins)?,
        CoreTool::Complete(args) => {
            storage::cases::update_outcome(connection, &case.id, &args.summary, args.result.as_ref(), now)?;
            ToolExecution::ending(json!({ "status": "completed" }), CaseState::Completed)
        }
        CoreTool::Fail(args) => {
            storage::cases::update_outcome(connection, &case.id, &args.reason, None, now)?;
            ToolExecution::ending(json!({ "status": "failed" }), CaseState::Failed)
        }
        CoreTool::NoteSet(args) => {
            storage::notes::set_note(connection, &case.id, &args.key, &args.value, now)?;
            ToolExecution::ok(json!({ "saved": args.key }))
        }
        CoreTool::NoteDelete(args) => {
            let existed = storage::notes::delete_note(connection, &case.id, &args.key)?;
            ToolExecution::ok(json!({ "deleted": existed }))
        }
        CoreTool::FindContact(args) => ToolExecution::ok(crate::contacts::find(connection, &args)?),
        CoreTool::SaveContact(args) => match crate::contacts::save(connection, &args, now)? {
            Ok(content) => ToolExecution::ok(content),
            Err(message) => ToolExecution::error(message),
        },
        CoreTool::ReadFile(args) => read_file(env, &args),
        CoreTool::ViewImage(args) => view_image(env, &args.file),
        CoreTool::ReadGuide(args) => read_guide(env.guides, &args.name),
        CoreTool::LoadPlugin(args) => load_plugin(env.plugins, &args.name),
    })
}

/// Build the request for the next LLM turn from the case and its event log.
fn build_request(
    connection: &Connection,
    prompts: &PromptSet,
    case: &Case,
    events: &[Event],
    model: String,
    vision: bool,
    shared: &Shared,
) -> Result<CompletionRequest> {
    let (store, plugins) = (&shared.files, &shared.plugin_tools);
    let notes = storage::notes::list_notes(connection, &case.id)?;
    let instructions = storage::instructions::list_instructions(connection, &case.id)?;
    let files = storage::files::list_files(connection, &case.id)?;
    let file_views = views(&files, vision);
    let guides = plugins.guides();
    let user_prompt = shared.settings.user_prompt_path.as_deref().and_then(crate::user_prompt::read);
    let loaded = plugins.loaded(events);
    let index = plugins.index(&loaded);
    let mut tools = core_tool_specs(!files.is_empty(), vision, !guides.is_empty());
    if !index.is_empty() {
        tools.push(load_plugin_spec());
    }
    describe_plugin_conditions(&mut tools, plugins, &loaded);
    tools.extend(plugins.specs(&loaded));
    let context = PromptContext {
        now: Utc::now().to_rfc3339(),
        case: CaseView {
            title: &case.title,
            goal: &case.goal,
            owner: case.owner.as_deref(),
            created_at: case.created_at.to_rfc3339(),
        },
        budgets: &case.budgets,
        usage: &case.usage,
        notes: &notes,
        tools: &tools,
        instructions: &instructions,
        files: &file_views,
        guides: &guides,
        plugins: &index,
        user_prompt: user_prompt.as_deref(),
        wake: None,
    };
    let mut sections = vec![prompts.render(SYSTEM, &context)?];
    if user_prompt.is_some() {
        sections.push(prompts.render(USER_PROMPT, &context)?);
    }
    // A profile removed from disk after the case was created is skipped, not fatal.
    match case.profile.as_deref() {
        Some(profile) if prompts.has_profile(profile) => sections.push(prompts.render_profile(profile, &context)?),
        Some(profile) => tracing::warn!(case_id = %case.id, profile, "profile no longer exists"),
        None => {}
    }
    sections.push(prompts.render(CASE_HEADER, &context)?);
    if !instructions.is_empty() {
        sections.push(prompts.render(INSTRUCTIONS, &context)?);
    }
    if !files.is_empty() {
        sections.push(prompts.render(FILES, &context)?);
    }
    if !guides.is_empty() {
        sections.push(prompts.render(GUIDES, &context)?);
    }
    if !index.is_empty() {
        sections.push(prompts.render(PLUGINS, &context)?);
    }
    let image =
        |id: &clankjob_core::ids::FileId| files.iter().find(|file| &file.id == id).and_then(|file| store.image(file));
    Ok(CompletionRequest {
        model,
        system: sections.join("\n\n"),
        messages: build_messages(events, prompts, &context, &image)?,
        tools,
    })
}

/// One running activation.
struct Activation<'a> {
    shared: &'a Shared,
    connection: &'a mut Connection,
    prompts: Arc<PromptSet>,
    id: ActivationId,
    case_id: CaseId,
    attempts: u32,
    turns: u32,
    tokens: TokenUsage,
}

impl Activation<'_> {
    /// Finish the case as failed and stop.
    fn fail(&mut self, case: &Case, reason: &str) -> Result<Flow> {
        tracing::warn!(case_id = %case.id, reason, "case failed");
        let now = Utc::now();
        let transaction = begin_write(self.connection)?;
        storage::cases::update_outcome(&transaction, &case.id, reason, None, now)?;
        let body = EventBody::Error {
            message: reason.to_owned(),
        };
        storage::events::append_event(&transaction, &case.id, Some(&self.id), &body, now)?;
        change_state(&transaction, case, CaseState::Failed, Some(&self.id), now)?;
        storage::queue::finish(&transaction, &case.id, now)?;
        commit(transaction)?;
        Ok(Flow::Stop)
    }

    /// Give the case back to the queue after an LLM outage, or fail it after too many tries.
    fn requeue(&mut self, case: &Case, error: &LlmError) -> Result<Flow> {
        let attempt = self.attempts.saturating_add(1);
        if attempt >= self.shared.settings.max_attempts {
            return self.fail(case, &format!("LLM unavailable after {attempt} attempts: {error}"));
        }
        let now = Utc::now();
        let retry_at = later(now, self.shared.settings.requeue_delay.saturating_mul(attempt));
        let transaction = begin_write(self.connection)?;
        let body = EventBody::Error {
            message: format!("{error}; retrying at {}", retry_at.to_rfc3339()),
        };
        storage::events::append_event(&transaction, &case.id, Some(&self.id), &body, now)?;
        change_state(&transaction, case, CaseState::Pending, Some(&self.id), now)?;
        storage::queue::release_for_retry(&transaction, &case.id, retry_at)?;
        commit(transaction)?;
        Ok(Flow::Stop)
    }

    /// Run tool calls in order, each in its own transaction.
    ///
    /// When a call suspends or finishes the case, the remaining calls are recorded as not
    /// run, in the same transaction, so they are not picked up as pending later.
    fn run_tools(&mut self, calls: &[ToolCall]) -> Result<Flow> {
        for (index, call) in calls.iter().enumerate() {
            // A plugin tool may take a while (a download, a command), so it runs before the
            // write transaction, which would otherwise block every other writer.
            let plugin_call = self.run_plugin_tool(call);
            let now = Utc::now();
            let transaction = begin_write(self.connection)?;
            let case = load_case(&transaction, &self.case_id)?;
            if case.state != CaseState::Running {
                // Cancelled meanwhile; dropping the transaction rolls it back.
                return Ok(Flow::Stop);
            }
            let files = storage::files::list_files(&transaction, &case.id)?;
            let vision = self
                .shared
                .providers
                .get(&case.llm)
                .is_some_and(|provider| provider.supports_images());
            let guides = self.shared.plugin_tools.guides();
            let (execution, written) = match plugin_call {
                PluginCall::Ran(output) => plugin_result(&transaction, &case, &self.shared.files, &files, output)?,
                PluginCall::RanWithoutApproval(output, reason) => {
                    let (mut execution, written) =
                        plugin_result(&transaction, &case, &self.shared.files, &files, output)?;
                    // Shown in the timeline and to the agent: why nobody was asked.
                    if let Some(object) = execution.content.as_object_mut() {
                        object.insert("approval".to_owned(), Value::String(format!("not needed: {reason}")));
                    }
                    (execution, written)
                }
                PluginCall::NeedsApproval(summary) => (request_approval(&transaction, &case, call, &summary)?, None),
                PluginCall::Core => {
                    let env = FileEnv {
                        files: &files,
                        vision,
                        guides: &guides,
                        plugins: &self.shared.plugin_tools,
                    };
                    (execute(&transaction, &case, call, &env)?, None)
                }
            };
            let result = ToolResult {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                content: execution.content,
                is_error: execution.is_error,
            };
            let body = EventBody::ToolResult(result);
            storage::events::append_event(&transaction, &case.id, Some(&self.id), &body, now)?;
            let Some(end_state) = execution.end_state else {
                if let Err(error) = commit(transaction) {
                    if let Some(file_id) = written {
                        self.shared.files.remove(&file_id);
                    }
                    return Err(error.into());
                }
                continue;
            };
            for skipped in calls.iter().skip(index.saturating_add(1)) {
                let body = EventBody::ToolResult(ToolResult {
                    tool_call_id: skipped.id.clone(),
                    tool_name: skipped.name.clone(),
                    content: json!({ "error": format!("not run: `{}` already ended this activation", call.name) }),
                    is_error: true,
                });
                storage::events::append_event(&transaction, &case.id, Some(&self.id), &body, now)?;
            }
            change_state(&transaction, &case, end_state, Some(&self.id), now)?;
            storage::queue::finish(&transaction, &case.id, now)?;
            commit(transaction)?;
            return Ok(Flow::Stop);
        }
        Ok(Flow::Continue)
    }

    /// Run a call if it names a plugin tool that needs no approval.
    fn run_plugin_tool(&self, call: &ToolCall) -> PluginCall {
        if CORE_TOOL_NAMES.contains(&call.name.as_str()) {
            return PluginCall::Core;
        }
        let Some(tool) = self.shared.plugin_tools.get(&call.name) else {
            return PluginCall::Core;
        };
        if let Some(summary) = tool.approval_summary(&call.arguments) {
            if let Err(error) = tool.validate(&call.arguments) {
                return PluginCall::Ran(Err(error));
            }
            return match self.approval_skipped(tool.as_ref(), call) {
                None => PluginCall::NeedsApproval(summary),
                Some(reason) => {
                    tracing::info!(case_id = %self.case_id, tool = %call.name, %reason, "ran without approval");
                    PluginCall::RanWithoutApproval(self.run_timed(tool.as_ref(), &call.name, &call.arguments), reason)
                }
            };
        }
        PluginCall::Ran(self.run_timed(tool.as_ref(), &call.name, &call.arguments))
    }

    /// Why a call that normally needs approval can run without it, or `None` to ask.
    /// Follows the case's setting, which only the owner changes; by default the tool's
    /// own check decides (e.g. every recipient a trusted contact). Anything that fails
    /// means asking.
    fn approval_skipped(&self, tool: &dyn clankjob_core::tool::PluginTool, call: &ToolCall) -> Option<String> {
        let policy = match storage::cases::get_case(self.connection, &self.case_id) {
            Ok(Some(case)) => case.approvals,
            _ => ApprovalPolicy::Always,
        };
        match policy {
            ApprovalPolicy::Always => None,
            ApprovalPolicy::Never => Some("this case is set to never ask for approval".to_owned()),
            ApprovalPolicy::Default => {
                let trusted = match storage::contacts::trusted_emails(self.connection) {
                    Ok(trusted) => trusted,
                    Err(error) => {
                        tracing::warn!(case_id = %self.case_id, %error, "cannot read trusted contacts; asking");
                        return None;
                    }
                };
                match tool.approval_check(&call.arguments, &trusted) {
                    Ok(reason) => reason,
                    Err(error) => {
                        tracing::warn!(case_id = %self.case_id, tool = %call.name, %error, "approval check failed; asking");
                        None
                    }
                }
            }
        }
    }

    /// The case as plugin tools see it: its id, link and files. A database error becomes a
    /// tool error.
    fn tool_context(&self) -> std::result::Result<ToolContext, String> {
        let files = storage::files::list_files(self.connection, &self.case_id)
            .map_err(|error| format!("cannot list the case's files: {error}"))?;
        Ok(ToolContext {
            case_id: self.case_id.to_string(),
            case_url: self.shared.channels.case_url(&self.case_id),
            files: files
                .into_iter()
                .map(|file| CaseFileRef {
                    path: self.shared.files.path(&file.id),
                    name: file.name,
                    media_type: file.media_type,
                })
                .collect(),
        })
    }

    /// Run a plugin tool and log how long it took.
    fn run_timed(
        &self,
        tool: &dyn clankjob_core::tool::PluginTool,
        name: &str,
        arguments: &Value,
    ) -> std::result::Result<ToolOutput, String> {
        let started = std::time::Instant::now();
        let context = self.tool_context()?;
        let output = tool.run(arguments, &context);
        let elapsed_ms = started.elapsed().as_millis();
        match &output {
            Ok(_) => {
                tracing::info!(case_id = %self.case_id, tool = name, plugin = tool.plugin(), elapsed_ms, "plugin tool ran");
            }
            Err(error) => {
                tracing::warn!(case_id = %self.case_id, tool = name, plugin = tool.plugin(), elapsed_ms, %error, "plugin tool failed");
            }
        }
        output
    }

    /// Run the case's approved tool calls that have not run yet, recording each result as a
    /// wake event before the LLM's next turn.
    ///
    /// Each call is marked `running` in its own transaction before it runs, so it runs at
    /// most once: a call still `running` here was interrupted, and is reported as having an
    /// unknown outcome instead of being run again (an email could otherwise go out twice).
    fn run_approved(&mut self, case: &Case) -> Result<()> {
        for request in storage::human::unfinished_executions(self.connection, &case.id)? {
            let tool = request.tool.clone().unwrap_or_default();
            let output = if request.execution == Some(Execution::Running) {
                Err("the server stopped while this call was running, so its outcome is unknown; check (e.g. the Sent folder) before trying again".to_owned())
            } else {
                if !storage::human::set_execution(self.connection, &request.id, Execution::Pending, Execution::Running)?
                {
                    continue;
                }
                match self.shared.plugin_tools.get(&tool) {
                    Some(plugin_tool) => {
                        let arguments = request.args.clone().unwrap_or(Value::Null);
                        self.run_timed(plugin_tool.as_ref(), &tool, &arguments)
                    }
                    None => Err(format!(
                        "the tool `{tool}` is no longer available (its plugin was removed or failed to load)"
                    )),
                }
            };
            let now = Utc::now();
            let transaction = begin_write(self.connection)?;
            let files = storage::files::list_files(&transaction, &case.id)?;
            let (execution, written) = plugin_result(&transaction, case, &self.shared.files, &files, output)?;
            let reason = WakeReason::ApprovedCallFinished {
                request_id: request.id.clone(),
                tool,
                result: execution.content,
                is_error: execution.is_error,
            };
            storage::events::append_event(&transaction, &case.id, Some(&self.id), &EventBody::Wake(reason), now)?;
            storage::human::set_execution(&transaction, &request.id, Execution::Running, Execution::Done)?;
            if let Err(error) = commit(transaction) {
                if let Some(file_id) = written {
                    self.shared.files.remove(&file_id);
                }
                return Err(error.into());
            }
        }
        Ok(())
    }

    /// Call the LLM, retrying retryable errors with exponential backoff.
    fn complete(
        &self,
        provider: &dyn LlmProvider,
        request: &CompletionRequest,
    ) -> std::result::Result<CompletionResponse, LlmError> {
        let settings = &self.shared.settings;
        let mut backoff = settings.llm_retry_backoff;
        let mut attempt = 0;
        loop {
            match provider.complete(request) {
                Err(LlmError::Retryable(message))
                    if attempt < settings.llm_retries && !self.shared.is_shutting_down() =>
                {
                    tracing::warn!(case_id = %self.case_id, %message, attempt, "retrying LLM call");
                    thread::sleep(backoff);
                    backoff = backoff.saturating_mul(2);
                    attempt = attempt.saturating_add(1);
                }
                result => return result,
            }
        }
    }

    /// Record an LLM turn and its token usage.
    fn record_turn(&mut self, case: &Case, events: &[Event], response: CompletionResponse) -> Result<Flow> {
        let now = Utc::now();
        self.tokens.input_tokens = self.tokens.input_tokens.saturating_add(response.usage.input_tokens);
        self.tokens.output_tokens = self.tokens.output_tokens.saturating_add(response.usage.output_tokens);
        let mut usage = case.usage;
        usage.input_tokens = usage.input_tokens.saturating_add(response.usage.input_tokens);
        usage.output_tokens = usage.output_tokens.saturating_add(response.usage.output_tokens);
        let silent = response.message.tool_calls.is_empty();
        let text = response.message.text.clone();
        let transaction = begin_write(self.connection)?;
        storage::cases::update_usage(&transaction, &case.id, &usage, now)?;
        let body = EventBody::LlmMessage(response.message);
        storage::events::append_event(&transaction, &case.id, Some(&self.id), &body, now)?;
        if !silent {
            commit(transaction)?;
            // The requested tools run at the next step, as pending calls.
            return Ok(Flow::Continue);
        }
        let already_nudged = events.last().is_some_and(|event| event.body == EventBody::Nudge);
        if !already_nudged {
            storage::events::append_event(&transaction, &case.id, Some(&self.id), &EventBody::Nudge, now)?;
            commit(transaction)?;
            return Ok(Flow::Continue);
        }
        // Twice without a tool call: hand the decision to the owner (design §5).
        let question = text
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| "The agent stopped without deciding what to do next. How should it proceed?".to_owned());
        let args = AskHumanArgs {
            question,
            timeout: None,
            also_wait_for: Vec::new(),
        };
        let execution = ask_human(&transaction, case, &args, &self.shared.plugin_tools)?;
        change_state(
            &transaction,
            case,
            execution.end_state.unwrap_or(CaseState::WaitingForHuman),
            Some(&self.id),
            now,
        )?;
        storage::queue::finish(&transaction, &case.id, now)?;
        commit(transaction)?;
        Ok(Flow::Stop)
    }

    /// Run one step: pending tool calls if any, otherwise one LLM turn.
    fn step(&mut self) -> Result<Flow> {
        let case = load_case(self.connection, &self.case_id)?;
        if case.state != CaseState::Running || self.shared.is_shutting_down() {
            // Cancelled, or shutting down: the lease lapses and the case resumes after a
            // restart (startup clears leases).
            return Ok(Flow::Stop);
        }
        self.run_approved(&case)?;
        let events = storage::events::list_events(self.connection, &case.id, 0, None)?;
        let pending = pending_tool_calls(&events);
        if !pending.is_empty() {
            return self.run_tools(&pending);
        }
        let budgets = case.budgets;
        if case.usage.activations > budgets.max_activations {
            return self.fail(
                &case,
                &format!("budget exceeded: more than {} activations", budgets.max_activations),
            );
        }
        if case.usage.total_tokens() >= budgets.max_total_tokens {
            return self.fail(
                &case,
                &format!("budget exceeded: {} tokens used", case.usage.total_tokens()),
            );
        }
        if self.turns >= budgets.max_turns_per_activation {
            return self.fail(
                &case,
                &format!("budget exceeded: {} LLM turns in one activation", self.turns),
            );
        }
        let Some(provider) = self.shared.providers.get(&case.llm).map(Arc::clone) else {
            return self.fail(&case, &format!("LLM `{}` is not configured", case.llm));
        };
        let model = case.model.clone().unwrap_or_else(|| provider.default_model().to_owned());
        let vision = provider.supports_images();
        let request = build_request(
            self.connection,
            &self.prompts,
            &case,
            &events,
            model,
            vision,
            self.shared,
        )?;
        let lease_until = later(Utc::now(), self.shared.settings.lease);
        storage::queue::renew_lease(self.connection, &case.id, lease_until)?;
        self.turns = self.turns.saturating_add(1);
        match self.complete(provider.as_ref(), &request) {
            Ok(response) => self.record_turn(&case, &events, response),
            Err(error @ LlmError::Retryable(_)) => self.requeue(&case, &error),
            Err(LlmError::Fatal(message)) => self.fail(&case, &format!("LLM error: {message}")),
        }
    }
}

/// Run an activation for a claimed case until it stops.
///
/// # Arguments
///
/// * `shared` - Engine state
/// * `connection` - This worker's database connection
/// * `claimed` - The case, already marked running
///
/// # Errors
///
/// Returns an error if the database fails or a prompt cannot be rendered. The case keeps
/// its lease and is retried when the lease expires.
pub(crate) fn run(shared: &Shared, connection: &mut Connection, claimed: ClaimedCase) -> Result<()> {
    let prompts = shared.prompts();
    let id = ActivationId::generate();
    let mut hashes = prompts.hashes();
    // The owner's prompt shapes behaviour like a template does, so its version is recorded too.
    if let Some(text) = shared.settings.user_prompt_path.as_deref().and_then(crate::user_prompt::read) {
        hashes.insert(USER_PROMPT.to_owned(), crate::prompts::sha256_hex(text.as_bytes()));
    }
    storage::activations::start_activation(connection, &id, &claimed.case.id, &hashes, Utc::now())?;
    tracing::info!(case_id = %claimed.case.id, activation_id = %id, "activation started");
    let mut activation = Activation {
        shared,
        connection,
        prompts,
        id,
        case_id: claimed.case.id,
        attempts: claimed.attempts,
        turns: 0,
        tokens: TokenUsage::default(),
    };
    while activation.step()? == Flow::Continue {}
    let end_state = load_case(activation.connection, &activation.case_id)?.state;
    storage::activations::end_activation(
        activation.connection,
        &activation.id,
        end_state,
        activation.tokens,
        Utc::now(),
    )?;
    tracing::info!(case_id = %activation.case_id, activation_id = %activation.id, %end_state, "activation ended");
    Ok(())
}

#[cfg(test)]
mod tests {
    use clankjob_core::case::{Budgets, NewCase};
    use clankjob_core::human::HumanRequestStatus;
    use clankjob_core::llm::{AssistantMessage, Message};

    use super::*;
    use crate::test_support::{ScriptedProvider, TestDb, engine};
    use crate::{Engine, transitions};

    fn reply(calls: &[(&str, Value)]) -> CompletionResponse {
        CompletionResponse {
            message: AssistantMessage {
                text: None,
                tool_calls: calls
                    .iter()
                    .enumerate()
                    .map(|(index, (name, arguments))| ToolCall {
                        id: format!("call_{name}_{index}"),
                        name: (*name).to_owned(),
                        arguments: arguments.clone(),
                    })
                    .collect(),
            },
            usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 10,
            },
        }
    }

    fn text(content: &str) -> CompletionResponse {
        CompletionResponse {
            message: AssistantMessage {
                text: Some(content.to_owned()),
                tool_calls: Vec::new(),
            },
            usage: TokenUsage::default(),
        }
    }

    fn create(engine: &Engine, connection: &mut Connection, budgets: Budgets) -> Case {
        let new_case = NewCase {
            title: "Electrician quote".to_owned(),
            goal: "Get a quote from Bob".to_owned(),
            owner: Some("joe".to_owned()),
            profile: None,
            llm: "default".to_owned(),
            model: None,
            budgets,
            instructions: Vec::new(),
            human_channels: None,
            approvals: clankjob_core::case::ApprovalPolicy::default(),
        };
        engine.create_case(connection, &new_case).unwrap()
    }

    /// Claim the next case and run one activation on it, returning the case afterwards.
    fn activate(engine: &Engine, connection: &mut Connection) -> Case {
        let now = Utc::now();
        let claimed = transitions::claim_next(connection, now, later(now, std::time::Duration::from_mins(10)))
            .unwrap()
            .unwrap();
        let id = claimed.case.id.clone();
        run(&engine.shared, connection, claimed).unwrap();
        load_case(connection, &id).unwrap()
    }

    fn events(connection: &Connection, case: &Case) -> Vec<EventBody> {
        storage::events::list_events(connection, &case.id, 0, None)
            .unwrap()
            .into_iter()
            .map(|event| event.body)
            .collect()
    }

    /// A plugin tool that returns a transcript as a file, or JSON when asked for `info`.
    struct FakeTranscript {
        spec: clankjob_core::llm::ToolSpec,
    }

    impl clankjob_core::tool::PluginTool for FakeTranscript {
        fn plugin(&self) -> &'static str {
            "youtube"
        }

        fn spec(&self) -> &clankjob_core::llm::ToolSpec {
            &self.spec
        }

        fn run(&self, arguments: &Value, _context: &ToolContext) -> std::result::Result<ToolOutput, String> {
            match arguments.get("video").and_then(Value::as_str) {
                Some("info") => Ok(ToolOutput::Json(json!({ "title": "Earnings call" }))),
                Some(video) => Ok(ToolOutput::File {
                    name: "transcript.md".to_owned(),
                    content: format!("[00:00] Revenue grew 12% ({video})").into_bytes(),
                    summary: json!({ "video": video }),
                }),
                None => Err("`video` is required".to_owned()),
            }
        }
    }

    #[test]
    fn plugin_tools_store_long_output_as_files_and_guides_are_readable() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([
            Ok(reply(&[
                ("read_guide", json!({"name": "earnings-call"})),
                ("youtube_transcript", json!({"video": "abc"})),
                ("youtube_transcript", json!({"video": "abc"})),
                ("youtube_transcript", json!({"video": "info"})),
                ("youtube_transcript", json!({})),
            ])),
            Ok(reply(&[("read_file", json!({"file": "transcript (2).md"}))])),
            Ok(reply(&[("complete", json!({"summary": "Revenue grew 12%."}))])),
        ]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let spec = clankjob_core::llm::ToolSpec {
            name: "youtube_transcript".to_owned(),
            description: "Get a transcript.".to_owned(),
            parameters: json!({"type": "object"}),
        };
        let clash = clankjob_core::llm::ToolSpec {
            name: "sleep".to_owned(),
            ..spec.clone()
        };
        let guide = Guide {
            plugin: "youtube".to_owned(),
            name: "earnings-call".to_owned(),
            description: "Analysing an earnings call.".to_owned(),
            content: "Cite timestamps.".to_owned(),
        };
        let refused = engine.plugin_tools().replace(
            vec![Arc::new(FakeTranscript { spec }), Arc::new(FakeTranscript { spec: clash })],
            vec![guide],
            Vec::new(),
        );
        let case = create(&engine, &mut connection, Budgets::default());

        // Act
        let done = activate(&engine, &mut connection);

        // Assert
        assert_eq!(refused.len(), 1, "a plugin cannot take a core tool's name");
        assert_eq!(done.state, CaseState::Completed);
        let files = storage::files::list_files(&connection, &case.id).unwrap();
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, ["transcript.md", "transcript (2).md"]);
        let results: Vec<ToolResult> = events(&connection, &case)
            .into_iter()
            .filter_map(|body| match body {
                EventBody::ToolResult(result) => Some(result),
                _ => None,
            })
            .collect();
        assert_eq!(results[0].content["content"], "Cite timestamps.");
        assert_eq!(results[1].content["stored_as"], "transcript.md");
        assert_eq!(results[1].content["video"], "abc");
        assert_eq!(results[3].content, json!({ "title": "Earnings call" }));
        assert!(results[4].is_error);
        assert!(results[5].content["text"].as_str().unwrap().contains("Revenue grew 12%"));
        let requests = provider.requests.lock().unwrap();
        assert!(
            requests[0]
                .system
                .contains("`earnings-call` (youtube): Analysing an earnings call.")
        );
        let offered =
            |index: usize| -> Vec<String> { requests[index].tools.iter().map(|tool| tool.name.clone()).collect() };
        assert!(offered(0).contains(&"read_guide".to_owned()));
        assert!(!offered(0).contains(&"youtube_transcript".to_owned()), "not loaded yet");
        assert!(
            offered(1).contains(&"youtube_transcript".to_owned()),
            "reading its guide loaded it"
        );
    }

    #[test]
    fn plugin_tools_are_offered_once_their_plugin_is_loaded() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([
            Ok(reply(&[("load_plugin", json!({"name": "nope"}))])),
            Ok(reply(&[("load_plugin", json!({"name": "YouTube"}))])),
            Ok(reply(&[("complete", json!({"summary": "Done."}))])),
        ]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let tool = |name: &str| clankjob_core::llm::ToolSpec {
            name: name.to_owned(),
            description: "A tool.".to_owned(),
            parameters: json!({"type": "object"}),
        };
        engine.plugin_tools().replace(
            vec![
                Arc::new(FakeTranscript {
                    spec: tool("youtube_transcript"),
                }),
                Arc::new(FakeSend {
                    spec: tool("send_email"),
                    sent: std::sync::Mutex::default(),
                }),
            ],
            Vec::new(),
            Vec::new(),
        );
        let case = create(&engine, &mut connection, Budgets::default());

        // Act
        let done = activate(&engine, &mut connection);

        // Assert
        assert_eq!(done.state, CaseState::Completed);
        let results: Vec<ToolResult> = events(&connection, &case)
            .into_iter()
            .filter_map(|body| match body {
                EventBody::ToolResult(result) => Some(result),
                _ => None,
            })
            .collect();
        assert!(results[0].is_error);
        assert!(
            results[0].content["error"]
                .as_str()
                .unwrap()
                .contains("available: email, youtube")
        );
        assert_eq!(results[1].content["plugin"], "youtube");
        let requests = provider.requests.lock().unwrap();
        let offered =
            |index: usize| -> Vec<String> { requests[index].tools.iter().map(|tool| tool.name.clone()).collect() };
        assert!(offered(0).contains(&"load_plugin".to_owned()));
        assert!(!offered(1).contains(&"youtube_transcript".to_owned()));
        assert!(offered(2).contains(&"youtube_transcript".to_owned()));
        assert!(!offered(2).contains(&"send_email".to_owned()), "only the loaded plugin");
        assert!(
            requests[0]
                .system
                .contains("- `email`: tools send_email\n- `youtube`: tools youtube_transcript")
        );
        assert!(requests[2].system.contains("- `youtube` (loaded): tools youtube_transcript"));
    }

    /// A tool that needs approval and records what it was run with.
    struct FakeSend {
        spec: clankjob_core::llm::ToolSpec,
        sent: std::sync::Mutex<Vec<Value>>,
    }

    impl clankjob_core::tool::PluginTool for FakeSend {
        fn plugin(&self) -> &'static str {
            "email"
        }

        fn spec(&self) -> &clankjob_core::llm::ToolSpec {
            &self.spec
        }

        fn approval_summary(&self, arguments: &Value) -> Option<String> {
            Some(format!("Send an email to {}", arguments["to"].as_str().unwrap_or("?")))
        }

        fn validate(&self, arguments: &Value) -> std::result::Result<(), String> {
            arguments.get("to").map(drop).ok_or_else(|| "`to` is required".to_owned())
        }

        /// Like the email plugin's: no approval when the recipient is trusted.
        fn approval_check(&self, arguments: &Value, trusted: &[String]) -> std::result::Result<Option<String>, String> {
            let to = arguments["to"].as_str().unwrap_or_default();
            if to.contains("check-fails") {
                return Err("the mail server is down".to_owned());
            }
            Ok(trusted
                .iter()
                .any(|address| address == to)
                .then(|| format!("{to} is a trusted contact")))
        }

        fn run(&self, arguments: &Value, _context: &ToolContext) -> std::result::Result<ToolOutput, String> {
            self.sent.lock().unwrap().push(arguments.clone());
            Ok(ToolOutput::Json(json!({ "message_id": "<1@x>" })))
        }
    }

    fn with_send_tool(engine: &Engine) -> Arc<FakeSend> {
        let tool = Arc::new(FakeSend {
            spec: clankjob_core::llm::ToolSpec {
                name: "send_email".to_owned(),
                description: "Send an email.".to_owned(),
                parameters: json!({"type": "object"}),
            },
            sent: std::sync::Mutex::default(),
        });
        engine.plugin_tools().replace(
            vec![Arc::clone(&tool) as Arc<dyn clankjob_core::tool::PluginTool>],
            Vec::new(),
            Vec::new(),
        );
        tool
    }

    fn open_approval(connection: &Connection, case: &Case) -> clankjob_core::human::HumanRequest {
        storage::human::list_requests(connection, Some(HumanRequestStatus::Open), Some(&case.id))
            .unwrap()
            .remove(0)
    }

    fn verdict(decision: clankjob_core::human::Decision, args: Option<&Value>) -> storage::human::Verdict<'_> {
        storage::human::Verdict {
            decision,
            args,
            comment: Some("ok"),
            via: "web",
            responder: None,
        }
    }

    #[test]
    fn approvals_follow_the_case_setting_and_trusted_contacts() {
        use clankjob_core::contact::{ContactSource, NewContact};

        // (case setting, recipient, whether the owner is asked)
        let scenarios = [
            (ApprovalPolicy::Default, "robin@sparky.ca", false),
            (ApprovalPolicy::Default, "stranger@x.ca", true),
            (ApprovalPolicy::Default, "check-fails@x.ca", true),
            (ApprovalPolicy::Always, "robin@sparky.ca", true),
            (ApprovalPolicy::Never, "stranger@x.ca", false),
        ];
        for (policy, to, asked) in scenarios {
            // Arrange
            let test_db = TestDb::new();
            let mut connection = test_db.connect();
            let provider = ScriptedProvider::new([
                Ok(reply(&[("send_email", json!({"to": to}))])),
                Ok(reply(&[("complete", json!({"summary": "Done."}))])),
            ]);
            let engine = engine(&test_db, Arc::clone(&provider));
            let tool = with_send_tool(&engine);
            let robin = NewContact {
                name: "Robin".to_owned(),
                email: Some("robin@sparky.ca".to_owned()),
                trusted: true,
                ..NewContact::default()
            };
            storage::contacts::insert_contact(&connection, &robin, ContactSource::Owner, Utc::now()).unwrap();
            let case = create(&engine, &mut connection, Budgets::default());
            storage::cases::update_approvals(&connection, &case.id, policy, Utc::now()).unwrap();

            // Act
            let after = activate(&engine, &mut connection);

            // Assert
            let what = format!("{policy:?} to {to}");
            let sent = tool.sent.lock().unwrap().len();
            if asked {
                assert_eq!((after.state, sent), (CaseState::WaitingForHuman, 0), "{what}");
            } else {
                assert_eq!((after.state, sent), (CaseState::Completed, 1), "{what}");
                let noted = events(&connection, &case).iter().any(|body| matches!(body,
                    EventBody::ToolResult(result) if result.content["approval"].as_str().is_some_and(|note| note.starts_with("not needed: "))));
                assert!(noted, "{what}: the timeline says why nobody was asked");
            }
        }
    }

    #[test]
    fn a_tool_needing_approval_runs_once_approved_with_the_owners_edits() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([
            Ok(reply(&[
                ("send_email", json!({})),
                ("send_email", json!({"to": "bob@x.ca", "body": "Hi"})),
            ])),
            Ok(reply(&[("complete", json!({"summary": "Sent."}))])),
        ]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let tool = with_send_tool(&engine);
        let case = create(&engine, &mut connection, Budgets::default());

        // Act: the first call is invalid, the second waits for approval
        let waiting = activate(&engine, &mut connection);
        let approval = open_approval(&connection, &case);
        let nothing_sent_yet = tool.sent.lock().unwrap().len();
        let edited = json!({"to": "bob@x.ca", "body": "Hello Bob"});
        engine
            .decide_approval(
                &mut connection,
                &approval.id,
                verdict(clankjob_core::human::Decision::Approve, Some(&edited)),
            )
            .unwrap();
        let done = activate(&engine, &mut connection);

        // Assert
        assert_eq!(waiting.state, CaseState::WaitingForHuman);
        assert_eq!(
            (approval.question.as_str(), approval.tool.as_deref()),
            ("Send an email to bob@x.ca", Some("send_email"))
        );
        assert_eq!(nothing_sent_yet, 0);
        assert_eq!(*tool.sent.lock().unwrap(), std::slice::from_ref(&edited));
        assert_eq!(done.state, CaseState::Completed);
        let bodies = events(&connection, &case);
        assert!(
            matches!(&bodies[3], EventBody::ToolResult(result) if result.is_error),
            "invalid args fail before approval"
        );
        assert!(bodies.iter().any(|body| matches!(body,
            EventBody::Wake(WakeReason::ApprovedCallFinished { result, is_error: false, .. }) if result["message_id"] == "<1@x>")));
        let second_turn = &provider.requests.lock().unwrap()[1];
        let said = format!("{:?}", second_turn.messages);
        assert!(said.contains("approved your `send_email` call with changes") && said.contains("Hello Bob"));
        let request = storage::human::get_request(&connection, &approval.id).unwrap().unwrap();
        assert_eq!(request.execution, Some(Execution::Done));
    }

    #[test]
    fn a_rejected_call_never_runs_and_an_interrupted_one_is_not_repeated() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([
            Ok(reply(&[("send_email", json!({"to": "bob@x.ca"}))])),
            Ok(reply(&[("send_email", json!({"to": "bob@x.ca"}))])),
            Ok(reply(&[("complete", json!({"summary": "Stopped."}))])),
        ]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let tool = with_send_tool(&engine);
        let case = create(&engine, &mut connection, Budgets::default());

        // Act: reject the first request
        activate(&engine, &mut connection);
        let first = open_approval(&connection, &case);
        engine
            .decide_approval(
                &mut connection,
                &first.id,
                verdict(clankjob_core::human::Decision::Reject, None),
            )
            .unwrap();
        activate(&engine, &mut connection);
        // Approve the second, but pretend the server died while it was running
        let second = open_approval(&connection, &case);
        engine
            .decide_approval(
                &mut connection,
                &second.id,
                verdict(clankjob_core::human::Decision::Approve, None),
            )
            .unwrap();
        storage::human::set_execution(&connection, &second.id, Execution::Pending, Execution::Running).unwrap();
        activate(&engine, &mut connection);

        // Assert
        assert!(tool.sent.lock().unwrap().is_empty(), "neither call may run");
        assert!(events(&connection, &case).iter().any(|body| matches!(body,
            EventBody::Wake(WakeReason::ApprovedCallFinished { result, is_error: true, .. })
                if result["error"].as_str().unwrap().contains("outcome is unknown"))));
    }

    #[test]
    fn the_owners_prompt_is_in_every_system_prompt_and_its_version_recorded() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([Ok(reply(&[("complete", json!({"summary": "Done."}))]))]);
        let path = test_db.files_dir.with_file_name("user_prompt.md");
        let providers =
            std::collections::HashMap::from([("default".to_owned(), Arc::clone(&provider) as Arc<dyn LlmProvider>)]);
        let settings = crate::EngineSettings {
            workers: 1,
            user_prompt_path: Some(path.clone()),
            ..crate::EngineSettings::default()
        };
        let engine = Engine::new(
            test_db.db.clone(),
            providers,
            None,
            test_db.files_dir.clone(),
            crate::channels::Channels::default(),
            settings,
        );
        engine.set_user_prompt("Sign emails as Brad.").unwrap();
        let case = create(&engine, &mut connection, Budgets::default());

        // Act
        activate(&engine, &mut connection);

        // Assert
        let system = provider.requests.lock().unwrap()[0].system.clone();
        let rules_end = system.find("## The owner's standing instructions").unwrap();
        assert!(system[rules_end..].contains("Sign emails as Brad."));
        assert!(
            system.find("Get a quote from Bob").unwrap() > rules_end,
            "it comes before the case itself"
        );
        let recorded: String = connection
            .query_row(
                "SELECT prompt_hashes FROM activations WHERE case_id = ?1",
                [case.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(recorded.contains("\"user_prompt\""));
        let (saved, _) = engine.user_prompt().unwrap().unwrap();
        assert_eq!(saved, "Sign emails as Brad.");
        assert!(matches!(
            engine.set_user_prompt(&"x".repeat(crate::user_prompt::MAX_USER_PROMPT_CHARS + 1)),
            Err(crate::EngineError::InvalidUserPrompt(_))
        ));
    }

    #[test]
    fn sleep_then_timer_then_complete() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([
            Ok(reply(&[
                ("note_set", json!({"key": "electrician", "value": "bob@sparky.ca"})),
                (
                    "sleep",
                    json!({"conditions": [{"kind": "core.timer", "params": {"after": "0s"}}], "reason": "wait"}),
                ),
            ])),
            Ok(reply(&[(
                "complete",
                json!({"summary": "Quoted $1450", "result": {"price": 1450}}),
            )])),
        ]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let case = create(&engine, &mut connection, Budgets::default());

        // Act: first activation goes to sleep
        let asleep = activate(&engine, &mut connection);

        // Assert
        assert_eq!(asleep.state, CaseState::Sleeping);
        assert_eq!(
            storage::notes::list_notes(&connection, &case.id).unwrap()[0].value,
            "bob@sparky.ca"
        );
        assert_eq!(storage::waits::active_waits(&connection, &case.id).unwrap().len(), 1);

        // Act: the timer fires and the second activation completes
        let fired = crate::scheduler::tick(&mut connection, Utc::now(), 10).unwrap();
        let done = activate(&engine, &mut connection);

        // Assert
        assert_eq!(fired, 1);
        assert_eq!(done.state, CaseState::Completed);
        assert_eq!(done.outcome.as_deref(), Some("Quoted $1450"));
        assert_eq!(done.result, Some(json!({"price": 1450})));
        assert_eq!(done.usage.activations, 2);
        assert_eq!(done.usage.input_tokens, 200);
        let requests = provider.requests.lock().unwrap();
        let second = requests.last().unwrap();
        assert!(second.system.contains("- electrician: bob@sparky.ca"));
        assert!(
            matches!(second.messages.last(), Some(Message::User { text, .. }) if text.contains("`core.timer` fired"))
        );
    }

    #[test]
    fn two_silent_turns_become_a_question_and_the_answer_resumes_the_case() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([
            Ok(text("Thinking...")),
            Ok(text("Should I accept the quote?")),
            Ok(reply(&[("complete", json!({"summary": "Accepted"}))])),
        ]);
        let engine = engine(&test_db, provider);
        let case = create(&engine, &mut connection, Budgets::default());

        // Act
        let waiting = activate(&engine, &mut connection);

        // Assert
        assert_eq!(waiting.state, CaseState::WaitingForHuman);
        let open = storage::human::list_requests(&connection, Some(HumanRequestStatus::Open), Some(&case.id)).unwrap();
        assert_eq!(open[0].question, "Should I accept the quote?");

        // Act: the owner answers
        engine.post_message(&mut connection, &case.id, "Yes").unwrap();
        let done = activate(&engine, &mut connection);

        // Assert
        assert_eq!(done.state, CaseState::Completed);
        assert!(events(&connection, &case).iter().any(|body| matches!(
            body,
            EventBody::Wake(clankjob_core::event::WakeReason::HumanAnswer { answer, .. }) if answer == "Yes"
        )));
    }

    #[test]
    fn a_model_changed_while_the_case_waits_is_used_from_the_next_turn() {
        // Arrange: a case waiting for an answer, on the LLM's default model
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([
            Ok(text("Thinking...")),
            Ok(text("Should I accept the quote?")),
            Ok(reply(&[("complete", json!({"summary": "Accepted"}))])),
        ]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let case = create(&engine, &mut connection, Budgets::default());
        activate(&engine, &mut connection);

        // Act: the owner switches models, then answers
        let changed = engine
            .set_model(
                &mut connection,
                &case.id,
                None,
                &clankjob_core::case::ModelChange::Model("big/model".to_owned()),
            )
            .unwrap();
        let unknown = engine.set_model(
            &mut connection,
            &case.id,
            Some("nope"),
            &clankjob_core::case::ModelChange::Keep,
        );
        engine.post_message(&mut connection, &case.id, "Yes").unwrap();
        let done = activate(&engine, &mut connection);

        // Assert
        assert_eq!(changed.model.as_deref(), Some("big/model"));
        assert!(matches!(unknown, Err(crate::EngineError::UnknownLlm(llm)) if llm == "nope"));
        assert_eq!(done.state, CaseState::Completed);
        let models: Vec<String> = provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.model.clone())
            .collect();
        assert_eq!(models, ["scripted", "scripted", "big/model"]);
    }

    #[test]
    fn files_added_mid_case_wake_it_and_are_readable_and_viewable() {
        // Arrange: a sleeping case, then three files arrive.
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let sleep = json!({"conditions": [{"kind": "core.timer", "params": {"after": "1d"}}], "reason": "wait"});
        let provider = ScriptedProvider::with_vision([
            Ok(reply(&[("sleep", sleep)])),
            Ok(reply(&[(
                "read_file",
                json!({"file": "contract.txt", "offset": 5, "max_chars": 10}),
            )])),
            Ok(reply(&[("view_image", json!({"file": "PANEL.png"}))])),
            Ok(reply(&[("complete", json!({"summary": "done"}))])),
        ]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let case = create(&engine, &mut connection, Budgets::default());
        assert_eq!(activate(&engine, &mut connection).state, CaseState::Sleeping);
        let long = format!("start{}", "x".repeat(20_000));
        engine
            .add_file(&mut connection, &case.id, "tone.md", b"Never offer more than $1,500.")
            .unwrap();
        engine
            .add_file(&mut connection, &case.id, "contract.txt", long.as_bytes())
            .unwrap();
        engine
            .add_file(
                &mut connection,
                &case.id,
                "uploads/panel.png",
                b"\x89PNG\r\n\x1a\nbytes",
            )
            .unwrap();

        // Act
        let done = activate(&engine, &mut connection);

        // Assert
        assert_eq!(done.state, CaseState::Completed);
        let requests = provider.requests.lock().unwrap();
        let first = &requests[1];
        // Files are listed, never pasted into the prompt, however small.
        assert!(first.system.contains("- `tone.md`: text, 29 B. Read it with `read_file`."));
        assert!(
            first
                .system
                .contains("- `contract.txt`: text, 20 KB. Read it with `read_file`.")
        );
        assert!(!first.system.contains("Never offer more than $1,500."));
        assert!(first.tools.iter().any(|tool| tool.name == "view_image"));
        let wakes = first
            .messages
            .iter()
            .filter(|message| matches!(message, Message::User { text, .. } if text.contains("The owner added a file")));
        assert_eq!(wakes.count(), 3);
        assert!(matches!(
            requests[2].messages.last(),
            Some(Message::Tool { content, .. }) if content.contains(r#""text":"xxxxxxxxxx""#) && content.contains(r#""next_offset":15"#)
        ));
        assert!(matches!(
            requests[3].messages.last(),
            Some(Message::User { images, .. }) if images.first().is_some_and(|image| image.media_type == "image/png")
        ));
    }

    fn new_case_with(instructions: Vec<clankjob_core::case::NewInstruction>) -> NewCase {
        NewCase {
            title: "Quote".to_owned(),
            goal: "Get a quote".to_owned(),
            owner: None,
            profile: None,
            llm: "default".to_owned(),
            model: None,
            budgets: Budgets::default(),
            instructions,
            human_channels: None,
            approvals: clankjob_core::case::ApprovalPolicy::default(),
        }
    }

    fn instruction(name: &str, content: &str) -> clankjob_core::case::NewInstruction {
        clankjob_core::case::NewInstruction {
            name: name.to_owned(),
            content: content.to_owned(),
        }
    }

    #[test]
    fn instructions_are_followed_from_the_first_run_and_edits_wake_the_case() {
        // Arrange: created with one instruction, then the agent sleeps.
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let sleep = json!({"conditions": [{"kind": "core.timer", "params": {"after": "1d"}}], "reason": "wait"});
        let provider = ScriptedProvider::new([
            Ok(reply(&[("sleep", sleep.clone())])),
            Ok(reply(&[("sleep", sleep)])),
            Ok(reply(&[("complete", json!({"summary": "done"}))])),
        ]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let case = engine
            .create_case(
                &mut connection,
                &new_case_with(vec![instruction("tone.md", "Be polite.")]),
            )
            .unwrap();
        activate(&engine, &mut connection);
        let tone = storage::instructions::list_instructions(&connection, &case.id)
            .unwrap()
            .remove(0);

        // Act: edit it, which wakes the case; then add and remove another.
        engine
            .change_instruction(
                &mut connection,
                &case.id,
                Some(&tone.id),
                Some(&instruction("tone.md", "Be firm.")),
            )
            .unwrap();
        activate(&engine, &mut connection);
        let extra = engine
            .change_instruction(
                &mut connection,
                &case.id,
                None,
                Some(&instruction("budget.md", "Max $1,500.")),
            )
            .unwrap()
            .unwrap();
        engine
            .change_instruction(&mut connection, &case.id, Some(&extra.id), None)
            .unwrap();
        activate(&engine, &mut connection);

        // Assert
        let requests = provider.requests.lock().unwrap();
        assert!(requests[0].system.contains("## Instructions from the owner"));
        assert!(requests[0].system.contains("### tone.md\n\nBe polite."));
        assert!(requests[1].system.contains("### tone.md\n\nBe firm."));
        assert!(matches!(
            requests[1].messages.last(),
            Some(Message::User { text, .. }) if text.contains("The owner updated the instruction `tone.md`")
        ));
        assert!(!requests[2].system.contains("budget.md"));
        assert!(matches!(
            requests[2].messages.last(),
            Some(Message::User { text, .. }) if text.contains("The owner removed the instruction `budget.md`")
        ));
    }

    #[test]
    fn instruction_limits_are_enforced() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let engine = engine(&test_db, ScriptedProvider::new([]));
        let too_long = "x".repeat(crate::MAX_INSTRUCTION_CHARS + 1);
        let near_limit = "y".repeat(crate::MAX_INSTRUCTION_CHARS);

        let created = engine.create_case(&mut connection, &new_case_with(vec![instruction("big.md", &too_long)]));
        let case = engine
            .create_case(
                &mut connection,
                &new_case_with(vec![instruction("a.md", &near_limit), instruction("b.md", &near_limit)]),
            )
            .unwrap();
        let over_total =
            engine.change_instruction(&mut connection, &case.id, None, Some(&instruction("c.md", &near_limit)));
        let empty = engine.change_instruction(&mut connection, &case.id, None, Some(&instruction("d.md", "  ")));
        let missing = engine.change_instruction(
            &mut connection,
            &case.id,
            Some(&clankjob_core::ids::InstructionId::generate()),
            None,
        );

        assert!(matches!(created, Err(crate::EngineError::InvalidInstruction(_))));
        assert!(
            matches!(over_total, Err(crate::EngineError::InvalidInstruction(message)) if message.contains("use files"))
        );
        assert!(matches!(empty, Err(crate::EngineError::InvalidInstruction(_))));
        assert!(matches!(missing, Err(crate::EngineError::InstructionNotFound(_))));
    }

    #[test]
    fn bad_files_and_cancelled_cases_are_refused() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let engine = engine(&test_db, ScriptedProvider::new([]));
        let case = create(&engine, &mut connection, Budgets::default());

        let binary = engine.add_file(&mut connection, &case.id, "app.exe", b"MZ\x90\x00\xff\xfe");
        let empty = engine.add_file(&mut connection, &case.id, "empty.txt", b"");
        engine.cancel_case(&mut connection, &case.id).unwrap();
        let cancelled = engine.add_file(&mut connection, &case.id, "late.txt", b"too late");

        assert!(matches!(binary, Err(crate::EngineError::InvalidFile(_))));
        assert!(matches!(empty, Err(crate::EngineError::InvalidFile(_))));
        assert!(matches!(cancelled, Err(crate::EngineError::InvalidState { .. })));
        assert_eq!(
            storage::files::list_files(&connection, &case.id).unwrap(),
            [] as [clankjob_core::file::CaseFile; 0]
        );
        assert_eq!(std::fs::read_dir(&test_db.files_dir).map_or(0, Iterator::count), 0);
    }

    #[test]
    fn view_image_is_refused_without_vision() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([
            Ok(reply(&[("view_image", json!({"file": "panel.png"}))])),
            Ok(reply(&[("complete", json!({"summary": "done"}))])),
        ]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let case = create(&engine, &mut connection, Budgets::default());
        engine
            .add_file(&mut connection, &case.id, "panel.png", b"\x89PNG\r\n\x1a\n")
            .unwrap();

        activate(&engine, &mut connection);

        let requests = provider.requests.lock().unwrap();
        assert!(!requests[0].tools.iter().any(|tool| tool.name == "view_image"));
        assert!(requests[0].system.contains("cannot see images"));
        assert!(matches!(
            requests[1].messages.last(),
            Some(Message::Tool { content, .. }) if content.contains("cannot see images")
        ));
    }

    #[test]
    fn calls_after_a_suspending_call_are_recorded_as_not_run() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([Ok(reply(&[
            ("complete", json!({"summary": "done"})),
            ("note_set", json!({"key": "k", "value": "v"})),
        ]))]);
        let engine = engine(&test_db, provider);
        let case = create(&engine, &mut connection, Budgets::default());

        let done = activate(&engine, &mut connection);

        assert_eq!(done.state, CaseState::Completed);
        assert_eq!(
            storage::notes::list_notes(&connection, &case.id).unwrap(),
            [] as [clankjob_core::case::CaseNote; 0]
        );
        let skipped = events(&connection, &case).into_iter().filter(
            |body| matches!(body, EventBody::ToolResult(result) if result.is_error && result.tool_name == "note_set"),
        );
        assert_eq!(skipped.count(), 1);
    }

    #[test]
    fn invalid_tool_arguments_are_reported_to_the_llm() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([
            Ok(reply(&[(
                "sleep",
                json!({"conditions": [{"kind": "email.reply"}], "reason": "wait"}),
            )])),
            Ok(reply(&[("fail", json!({"reason": "cannot wait for email"}))])),
        ]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let case = create(&engine, &mut connection, Budgets::default());

        let done = activate(&engine, &mut connection);

        assert_eq!(done.state, CaseState::Failed);
        assert_eq!(
            storage::waits::active_waits(&connection, &case.id).unwrap(),
            [] as [clankjob_core::wait::WaitCondition; 0]
        );
        let requests = provider.requests.lock().unwrap();
        assert!(matches!(
            requests.last().unwrap().messages.last(),
            Some(Message::Tool { content, .. }) if content.contains("unknown wait condition kind")
        ));
    }

    #[test]
    fn pending_calls_from_a_crashed_activation_run_without_calling_the_llm() {
        // Arrange: the LLM's turn was recorded but the process died before running it.
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([]);
        let engine = engine(&test_db, Arc::clone(&provider));
        let case = create(&engine, &mut connection, Budgets::default());
        let response = reply(&[("complete", json!({"summary": "done"}))]);
        let body = EventBody::LlmMessage(response.message);
        storage::events::append_event(&connection, &case.id, None, &body, Utc::now()).unwrap();

        // Act
        let done = activate(&engine, &mut connection);

        // Assert
        assert_eq!(done.state, CaseState::Completed);
        assert_eq!(provider.request_count(), 0);
    }

    #[test]
    fn retryable_errors_requeue_and_fatal_errors_fail() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let outage = || Err(LlmError::Retryable("503".to_owned()));
        let provider =
            ScriptedProvider::new([outage(), outage(), outage(), outage(), Err(LlmError::Fatal("401".to_owned()))]);
        let engine = engine(&test_db, Arc::clone(&provider));
        create(&engine, &mut connection, Budgets::default());

        // Act / Assert: four retryable errors (1 try + 3 retries) requeue the case
        assert_eq!(activate(&engine, &mut connection).state, CaseState::Pending);
        assert_eq!(provider.request_count(), 4);
        // The fatal error on the next attempt fails it
        let failed = activate(&engine, &mut connection);
        assert_eq!(failed.state, CaseState::Failed);
        assert_eq!(failed.outcome.as_deref(), Some("LLM error: 401"));
    }

    #[test]
    fn turn_budget_fails_the_case() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let note = || Ok(reply(&[("note_set", json!({"key": "k", "value": "v"}))]));
        let provider = ScriptedProvider::new([note(), note(), note()]);
        let engine = engine(&test_db, provider);
        create(
            &engine,
            &mut connection,
            Budgets {
                max_turns_per_activation: 2,
                ..Budgets::default()
            },
        );

        let failed = activate(&engine, &mut connection);

        assert_eq!(failed.state, CaseState::Failed);
        assert!(failed.outcome.unwrap().contains("2 LLM turns"));
    }

    #[test]
    fn ask_human_with_timeout_and_extra_condition_registers_both_waits() {
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([Ok(reply(&[(
            "ask_human",
            json!({
                "question": "Photo of the panel?",
                "timeout": "2d",
                "also_wait_for": [{"kind": "core.timer", "params": {"after": "1d"}}]
            }),
        )]))]);
        let engine = engine(&test_db, provider);
        let case = create(&engine, &mut connection, Budgets::default());

        let waiting = activate(&engine, &mut connection);

        assert_eq!(waiting.state, CaseState::WaitingForHuman);
        let kinds: Vec<String> = storage::waits::active_waits(&connection, &case.id)
            .unwrap()
            .into_iter()
            .map(|wait| wait.kind)
            .collect();
        assert_eq!(kinds.len(), 2);
        assert!(kinds.contains(&HUMAN_INPUT_KIND.to_owned()));
    }

    #[test]
    fn engine_threads_run_a_case_to_completion() {
        // Arrange
        let test_db = TestDb::new();
        let mut connection = test_db.connect();
        let provider = ScriptedProvider::new([
            Ok(reply(&[(
                "sleep",
                json!({"conditions": [{"kind": "core.timer", "params": {"after": "50ms"}}], "reason": "wait"}),
            )])),
            Ok(reply(&[("complete", json!({"summary": "done"}))])),
        ]);
        let engine = engine(&test_db, provider);
        let handles = engine.start().unwrap();
        let case = create(&engine, &mut connection, Budgets::default());

        // Act: wait up to 5 seconds for the worker and scheduler to finish the case
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut state = CaseState::Pending;
        while std::time::Instant::now() < deadline && state != CaseState::Completed {
            thread::sleep(std::time::Duration::from_millis(20));
            state = load_case(&connection, &case.id).unwrap().state;
        }
        engine.shutdown();
        for handle in handles {
            handle.join().unwrap();
        }

        // Assert
        assert_eq!(state, CaseState::Completed);
        assert!(engine.last_scheduler_tick().is_some());
    }
}
