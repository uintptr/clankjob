//! The clankjob case engine.
//!
//! [`Engine`] owns the worker threads that run activations and the scheduler thread that
//! fires due wait conditions. Its operations (create a case, post a message, answer a
//! question, …) are what the REST API calls.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use chrono::{DateTime, Utc};
use clankjob_core::case::{Case, CaseState, Instruction, NewCase, NewInstruction};
use clankjob_core::file::CaseFile;
use clankjob_core::human::HumanRequest;
use clankjob_core::ids::{CaseId, FileId, HumanRequestId, InstructionId};
use clankjob_core::llm::LlmProvider;
use clankjob_storage::human::Answer;
use clankjob_storage::{Connection, Db, StorageError};

mod activation;
pub mod channels;
mod checks;
pub mod context;
pub mod files;
pub mod plugin_tools;
pub mod prompts;
mod scheduler;
pub mod tools;
pub mod transitions;
pub mod user_prompt;

use channels::Channels;
use files::FileStore;
use plugin_tools::PluginTools;
use prompts::PromptSet;

/// Errors from engine operations.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The database failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// No case has this id.
    #[error("case {0} not found")]
    CaseNotFound(CaseId),
    /// No human request has this id.
    #[error("human request {0} not found")]
    RequestNotFound(HumanRequestId),
    /// The request is a question, not an approval.
    #[error("human request {0} is a question, not an approval")]
    NotAnApproval(HumanRequestId),
    /// The request is an approval: it takes a decision, not a text answer.
    #[error("human request {0} is an approval: send a decision (approve or reject)")]
    NotAQuestion(HumanRequestId),
    /// The request was already answered, superseded or cancelled.
    #[error("human request {0} is no longer open")]
    AlreadyResolved(HumanRequestId),
    /// The operation is not allowed in the case's current state.
    #[error("case {id} is {state}")]
    InvalidState {
        /// The case.
        id: CaseId,
        /// Its current state.
        state: CaseState,
    },
    /// The case names an LLM that is not configured.
    #[error("unknown LLM `{0}`")]
    UnknownLlm(String),
    /// The case names a profile that does not exist.
    #[error("unknown profile `{0}`")]
    UnknownProfile(String),
    /// The case names a human channel that is not configured.
    #[error("unknown human channel `{0}`")]
    UnknownChannel(String),
    /// No instruction of the case has this id.
    #[error("instruction {0} not found")]
    InstructionNotFound(InstructionId),
    /// An instruction is empty, too long, or over the case's limits.
    #[error("{0}")]
    InvalidInstruction(String),
    /// A file cannot be added: unsupported type, too large, or over the case's limits.
    #[error("{0}")]
    InvalidFile(String),
    /// A file's bytes could not be written or read.
    #[error("file storage failed: {0}")]
    FileStorage(#[from] std::io::Error),
    /// The user prompt is too long, or no location is configured for it.
    #[error("{0}")]
    InvalidUserPrompt(String),
    /// A prompt template failed to render.
    #[error(transparent)]
    Prompt(#[from] prompts::RenderError),
}

/// Shorthand for results of engine operations.
pub type Result<T> = std::result::Result<T, EngineError>;

/// Most instructions one case can hold.
pub const MAX_INSTRUCTIONS: usize = 10;
/// Longest single instruction, in characters.
pub const MAX_INSTRUCTION_CHARS: usize = 20_000;
/// Longest total of a case's instructions, in characters. They are resent with every
/// LLM turn, so this bounds what they cost.
pub const MAX_INSTRUCTIONS_CHARS: usize = 50_000;

/// Check a case's full set of instructions against the limits.
///
/// # Errors
///
/// Returns [`EngineError::InvalidInstruction`] explaining the first limit broken.
pub fn validate_instructions<'a, I>(instructions: I) -> Result<()>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let invalid = |message: String| Err(EngineError::InvalidInstruction(message));
    let (mut count, mut total) = (0_usize, 0_usize);
    for (name, content) in instructions {
        count = count.saturating_add(1);
        let chars = content.chars().count();
        total = total.saturating_add(chars);
        if name.trim().is_empty() || name.chars().count() > 200 {
            return invalid("instruction names must be 1 to 200 characters".to_owned());
        }
        if content.trim().is_empty() {
            return invalid(format!("instruction `{name}` is empty"));
        }
        if chars > MAX_INSTRUCTION_CHARS {
            return invalid(format!(
                "instruction `{name}` is longer than {MAX_INSTRUCTION_CHARS} characters"
            ));
        }
    }
    if count > MAX_INSTRUCTIONS {
        return invalid(format!("a case holds at most {MAX_INSTRUCTIONS} instructions"));
    }
    if total > MAX_INSTRUCTIONS_CHARS {
        return invalid(format!(
            "a case's instructions total at most {MAX_INSTRUCTIONS_CHARS} characters; use files for longer material"
        ));
    }
    Ok(())
}

/// Tuning knobs for the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSettings {
    /// Number of worker threads running activations.
    pub workers: usize,
    /// How long a claimed case stays reserved without a heartbeat.
    pub lease: Duration,
    /// How often the scheduler looks for due wait conditions.
    pub scheduler_tick: Duration,
    /// How long an idle worker waits before looking for work again, if not notified.
    pub idle_poll: Duration,
    /// Retries of a retryable LLM error within one activation.
    pub llm_retries: u32,
    /// Delay before the first in-activation retry; doubles each time.
    pub llm_retry_backoff: Duration,
    /// Base delay before a case whose LLM keeps failing is retried (times the attempt).
    pub requeue_delay: Duration,
    /// Attempts after which a case whose LLM keeps failing is marked failed.
    pub max_attempts: u32,
    /// Maximum wait conditions fired per scheduler tick.
    pub scheduler_batch: u32,
    /// The owner's own prompt, added to every case (`user_prompt.md` in the data
    /// directory); `None` for none.
    pub user_prompt_path: Option<PathBuf>,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            workers: 4,
            lease: Duration::from_mins(10),
            scheduler_tick: Duration::from_secs(15),
            idle_poll: Duration::from_secs(5),
            llm_retries: 3,
            llm_retry_backoff: Duration::from_secs(2),
            requeue_delay: Duration::from_secs(60),
            max_attempts: 5,
            scheduler_batch: 100,
            user_prompt_path: None,
        }
    }
}

/// Lets idle threads sleep until new work may be available.
///
/// A generation counter avoids the lost-wakeup race: a thread records the generation
/// before looking for work and only sleeps if nothing was notified since.
#[derive(Debug, Default)]
struct WorkSignal {
    generation: Mutex<u64>,
    condvar: Condvar,
}

impl WorkSignal {
    fn generation(&self) -> u64 {
        // A poisoned mutex only means another thread panicked while holding it; the
        // counter itself is still valid, so recover it instead of propagating the panic.
        *self.generation.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn notify(&self) {
        let mut generation = self.generation.lock().unwrap_or_else(PoisonError::into_inner);
        *generation = generation.wrapping_add(1);
        self.condvar.notify_all();
    }

    fn wait(&self, seen: u64, timeout: Duration) {
        let guard = self.generation.lock().unwrap_or_else(PoisonError::into_inner);
        // Returns early when notified; the result only says whether it timed out.
        let _ = self
            .condvar
            .wait_timeout_while(guard, timeout, |generation| *generation == seen);
    }
}

/// State shared by the engine handle and its threads.
struct Shared {
    db: Db,
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    prompts: RwLock<Arc<PromptSet>>,
    prompts_dir: Option<PathBuf>,
    files: FileStore,
    channels: Channels,
    plugin_tools: PluginTools,
    settings: EngineSettings,
    signal: WorkSignal,
    shutdown: AtomicBool,
    last_tick_millis: AtomicI64,
}

impl Shared {
    fn prompts(&self) -> Arc<PromptSet> {
        Arc::clone(&self.prompts.read().unwrap_or_else(PoisonError::into_inner))
    }

    fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

/// Handle to the case engine. Cheap to clone; clones share the same threads and state.
#[derive(Clone)]
pub struct Engine {
    shared: Arc<Shared>,
}

impl Engine {
    /// Create an engine. No thread runs until [`Engine::start`].
    ///
    /// # Arguments
    ///
    /// * `db` - The database (already migrated)
    /// * `providers` - Configured LLMs by name
    /// * `prompts_dir` - Directory with prompt overrides, if any
    /// * `files_dir` - Directory holding the bytes of files added to cases
    /// * `channels` - Human channels besides the web UI (design §10.3)
    /// * `settings` - Tuning knobs
    pub fn new(
        db: Db,
        providers: HashMap<String, Arc<dyn LlmProvider>>,
        prompts_dir: Option<PathBuf>,
        files_dir: PathBuf,
        channels: Channels,
        settings: EngineSettings,
    ) -> Self {
        let prompts = prompts_dir
            .as_ref()
            .map_or_else(PromptSet::builtin, |dir| PromptSet::load(dir, None));
        Self {
            shared: Arc::new(Shared {
                db,
                providers,
                prompts: RwLock::new(Arc::new(prompts)),
                prompts_dir,
                files: FileStore::new(files_dir),
                channels,
                plugin_tools: PluginTools::default(),
                settings,
                signal: WorkSignal::default(),
                shutdown: AtomicBool::new(false),
                last_tick_millis: AtomicI64::new(0),
            }),
        }
    }

    /// Start the worker threads, the scheduler thread, the channel thread and the thread
    /// checking plugin wait conditions.
    ///
    /// # Returns
    ///
    /// Handles of the started threads, to join after [`Engine::shutdown`]
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the operating system refuses to create a thread.
    pub fn start(&self) -> std::io::Result<Vec<JoinHandle<()>>> {
        let mut handles = Vec::with_capacity(self.shared.settings.workers.saturating_add(1));
        for index in 0..self.shared.settings.workers {
            let shared = Arc::clone(&self.shared);
            handles.push(
                thread::Builder::new()
                    .name(format!("worker-{index}"))
                    .spawn(move || worker_loop(&shared))?,
            );
        }
        let shared = Arc::clone(&self.shared);
        handles.push(
            thread::Builder::new()
                .name("scheduler".to_owned())
                .spawn(move || scheduler_loop(&shared))?,
        );
        // Always started: plugins can be loaded later, and they idle cheaply without them.
        let shared = Arc::clone(&self.shared);
        handles.push(
            thread::Builder::new()
                .name("channels".to_owned())
                .spawn(move || channels::channel_loop(&shared))?,
        );
        let shared = Arc::clone(&self.shared);
        handles.push(
            thread::Builder::new()
                .name("checks".to_owned())
                .spawn(move || checks::check_loop(&shared))?,
        );
        Ok(handles)
    }

    /// Ask every thread to stop after its current step.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.signal.notify();
    }

    /// The database.
    #[must_use]
    pub fn db(&self) -> &Db {
        &self.shared.db
    }

    /// The configured human channels.
    #[must_use]
    pub fn channels(&self) -> &Channels {
        &self.shared.channels
    }

    /// Tools and guides offered by plugins; swapped when plugins are reloaded.
    #[must_use]
    pub fn plugin_tools(&self) -> &PluginTools {
        &self.shared.plugin_tools
    }

    /// The owner's own prompt and when it was last saved; `None` when there is none yet.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::InvalidUserPrompt`] if no location is configured.
    pub fn user_prompt(&self) -> Result<Option<(String, DateTime<Utc>)>> {
        let path = self.user_prompt_path()?;
        let modified = std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(DateTime::<Utc>::from);
        Ok(user_prompt::read(path).zip(modified))
    }

    /// Replace the owner's own prompt; every case uses it from its next turn. An empty
    /// text removes it.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::InvalidUserPrompt`] if it is too long or no location is
    /// configured, or [`EngineError::FileStorage`] if it cannot be written.
    pub fn set_user_prompt(&self, content: &str) -> Result<()> {
        let path = self.user_prompt_path()?;
        user_prompt::validate(content).map_err(EngineError::InvalidUserPrompt)?;
        user_prompt::write(path, content)?;
        tracing::info!(path = %path.display(), chars = content.trim().chars().count(), "user prompt saved");
        Ok(())
    }

    fn user_prompt_path(&self) -> Result<&std::path::Path> {
        self.shared
            .settings
            .user_prompt_path
            .as_deref()
            .ok_or_else(|| EngineError::InvalidUserPrompt("no location is configured for the user prompt".to_owned()))
    }

    /// The prompt templates currently in use.
    #[must_use]
    pub fn prompts(&self) -> Arc<PromptSet> {
        self.shared.prompts()
    }

    /// Reload prompt templates from the prompts directory (design §7.4).
    ///
    /// # Returns
    ///
    /// The new prompt set; check [`PromptSet::errors`] for rejected files
    pub fn reload_prompts(&self) -> Arc<PromptSet> {
        let current = self.shared.prompts();
        let reloaded = Arc::new(match &self.shared.prompts_dir {
            Some(dir) => PromptSet::load(dir, Some(&current)),
            None => PromptSet::builtin(),
        });
        *self.shared.prompts.write().unwrap_or_else(PoisonError::into_inner) = Arc::clone(&reloaded);
        reloaded
    }

    /// When the scheduler last completed a tick, if ever.
    #[must_use]
    pub fn last_scheduler_tick(&self) -> Option<DateTime<Utc>> {
        match self.shared.last_tick_millis.load(Ordering::SeqCst) {
            0 => None,
            millis => DateTime::from_timestamp_millis(millis),
        }
    }

    /// Create a case and queue its first activation.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::UnknownLlm`], [`EngineError::UnknownProfile`] or
    /// [`EngineError::UnknownChannel`] if the case refers to something that is not
    /// configured, [`EngineError::InvalidInstruction`] if its
    /// instructions break a limit, or [`EngineError::Storage`].
    pub fn create_case(&self, connection: &mut Connection, new_case: &NewCase) -> Result<Case> {
        if !self.shared.providers.contains_key(&new_case.llm) {
            return Err(EngineError::UnknownLlm(new_case.llm.clone()));
        }
        if let Some(profile) = new_case
            .profile
            .as_deref()
            .filter(|profile| !self.prompts().has_profile(profile))
        {
            return Err(EngineError::UnknownProfile(profile.to_owned()));
        }
        validate_instructions(new_case.instructions.iter().map(|i| (i.name.as_str(), i.content.as_str())))?;
        let human_channels = self.shared.channels.resolve(new_case.human_channels.as_deref())?;
        let new_case = NewCase {
            human_channels: Some(human_channels),
            ..new_case.clone()
        };
        let case = transitions::create_case(connection, &new_case, Utc::now())?;
        self.shared.signal.notify();
        Ok(case)
    }

    /// Add a file to a case and wake the case so its agent sees it (design §7.5).
    ///
    /// # Arguments
    ///
    /// * `connection` - Database connection
    /// * `case_id` - The case
    /// * `name` - Original file name; only its last path component is kept
    /// * `bytes` - The content; its type is detected from the bytes
    ///
    /// # Returns
    ///
    /// The stored file
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::InvalidFile`] for an unsupported, empty or oversized file or a
    /// case over its file limits, [`EngineError::CaseNotFound`], [`EngineError::InvalidState`]
    /// for a cancelled case, or a storage error.
    pub fn add_file(
        &self,
        connection: &mut Connection,
        case_id: &CaseId,
        name: &str,
        bytes: &[u8],
    ) -> Result<CaseFile> {
        let file = prepare_file(connection, case_id, name, bytes)?;
        // Bytes first: a row must never point at a missing file. If the row cannot be
        // written, the orphaned bytes are removed again.
        self.shared.files.write(&file.id, bytes)?;
        if let Err(error) = transitions::add_file(connection, &file, Utc::now()) {
            self.shared.files.remove(&file.id);
            return Err(error);
        }
        self.shared.signal.notify();
        Ok(file)
    }

    /// Add, edit (`id` set) or remove (`instruction` unset) an instruction, and wake the case.
    ///
    /// # Arguments
    ///
    /// * `connection` - Database connection
    /// * `case_id` - The case
    /// * `id` - The instruction to edit or remove; `None` to add one
    /// * `instruction` - The new name and text; `None` to remove
    ///
    /// # Returns
    ///
    /// The instruction as stored, or `None` after a removal
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::InvalidInstruction`] if the result breaks a limit,
    /// [`EngineError::InstructionNotFound`], [`EngineError::CaseNotFound`],
    /// [`EngineError::InvalidState`] for a cancelled case, or [`EngineError::Storage`].
    pub fn change_instruction(
        &self,
        connection: &mut Connection,
        case_id: &CaseId,
        id: Option<&InstructionId>,
        instruction: Option<&NewInstruction>,
    ) -> Result<Option<Instruction>> {
        let existing = clankjob_storage::instructions::list_instructions(connection, case_id)?;
        if let Some(id) = id
            && !existing.iter().any(|stored| &stored.id == id)
        {
            return Err(EngineError::InstructionNotFound(id.clone()));
        }
        if let Some(instruction) = instruction {
            let kept = existing
                .iter()
                .filter(|stored| Some(&stored.id) != id)
                .map(|stored| (stored.name.as_str(), stored.content.as_str()));
            validate_instructions(kept.chain(std::iter::once((
                instruction.name.as_str(),
                instruction.content.as_str(),
            ))))?;
        }
        let stored = transitions::change_instruction(connection, case_id, id, instruction, Utc::now())?;
        self.shared.signal.notify();
        Ok(stored)
    }

    /// Where file bytes are stored, for serving them back.
    #[must_use]
    pub fn files(&self) -> &FileStore {
        &self.shared.files
    }

    /// Post a message from the owner; see [`transitions::post_message`].
    ///
    /// # Errors
    ///
    /// See [`transitions::post_message`].
    pub fn post_message(&self, connection: &mut Connection, case_id: &CaseId, text: &str) -> Result<()> {
        transitions::post_message(connection, case_id, text, Utc::now())?;
        self.shared.signal.notify();
        Ok(())
    }

    /// Answer a human request; see [`transitions::answer_request`].
    ///
    /// # Errors
    ///
    /// See [`transitions::answer_request`].
    pub fn answer_request(
        &self,
        connection: &mut Connection,
        request_id: &HumanRequestId,
        answer: Answer<'_>,
    ) -> Result<HumanRequest> {
        let request = transitions::answer_request(connection, request_id, answer, Utc::now())?;
        self.shared.signal.notify();
        Ok(request)
    }

    /// Decide an approval; see [`transitions::decide_approval`].
    ///
    /// # Errors
    ///
    /// See [`transitions::decide_approval`].
    pub fn decide_approval(
        &self,
        connection: &mut Connection,
        request_id: &HumanRequestId,
        verdict: clankjob_storage::human::Verdict<'_>,
    ) -> Result<HumanRequest> {
        let request = transitions::decide_approval(connection, request_id, verdict, Utc::now())?;
        self.shared.signal.notify();
        Ok(request)
    }

    /// Wake a case by hand; see [`transitions::wake_case`].
    ///
    /// # Errors
    ///
    /// See [`transitions::wake_case`].
    pub fn wake_case(&self, connection: &mut Connection, case_id: &CaseId) -> Result<()> {
        transitions::wake_case(connection, case_id, Utc::now())?;
        self.shared.signal.notify();
        Ok(())
    }

    /// Cancel a case; see [`transitions::cancel_case`].
    ///
    /// # Errors
    ///
    /// See [`transitions::cancel_case`].
    pub fn cancel_case(&self, connection: &mut Connection, case_id: &CaseId) -> Result<()> {
        transitions::cancel_case(connection, case_id, Utc::now())
    }
}

/// Check a new file against the size and per-case limits and inspect its content
/// (design §7.5). Nothing is stored.
///
/// # Errors
///
/// Returns [`EngineError::InvalidFile`] for an unsupported, empty or oversized file or a
/// case over its file limits, or [`EngineError::Storage`].
pub(crate) fn prepare_file(connection: &Connection, case_id: &CaseId, name: &str, bytes: &[u8]) -> Result<CaseFile> {
    let name = files::clean_name(name).map_err(EngineError::InvalidFile)?;
    if bytes.is_empty() || bytes.len() > files::MAX_FILE_BYTES {
        return Err(EngineError::InvalidFile(format!(
            "`{name}` must be between 1 byte and {} MB",
            files::MAX_FILE_BYTES / 1024 / 1024
        )));
    }
    let existing = clankjob_storage::files::list_files(connection, case_id)?;
    let total: u64 = existing.iter().map(|file| file.size).sum();
    let size = bytes.len() as u64;
    if existing.len() >= files::MAX_CASE_FILES || total.saturating_add(size) > files::MAX_CASE_FILE_BYTES {
        return Err(EngineError::InvalidFile(format!(
            "a case holds at most {} files and {} MB",
            files::MAX_CASE_FILES,
            files::MAX_CASE_FILE_BYTES / 1024 / 1024
        )));
    }
    let inspected = files::inspect(&name, bytes).map_err(EngineError::InvalidFile)?;
    let file = CaseFile {
        id: FileId::generate(),
        case_id: case_id.clone(),
        name,
        media_type: inspected.media_type,
        kind: inspected.kind,
        size,
        sha256: prompts::sha256_hex(bytes),
        text_chars: inspected.text.as_ref().map(|text| text.chars().count() as u64),
        text: inspected.text,
        pages: inspected.pages,
        created_at: Utc::now(),
    };
    Ok(file)
}

/// `time + duration`, saturating at the latest representable time instead of overflowing.
fn later(time: DateTime<Utc>, duration: Duration) -> DateTime<Utc> {
    chrono::Duration::from_std(duration)
        .ok()
        .and_then(|duration| time.checked_add_signed(duration))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

fn worker_loop(shared: &Shared) {
    let mut connection = match shared.db.connect() {
        Ok(connection) => connection,
        Err(error) => {
            tracing::error!(%error, "worker cannot open the database");
            return;
        }
    };
    while !shared.is_shutting_down() {
        let seen = shared.signal.generation();
        let now = Utc::now();
        match transitions::claim_next(&mut connection, now, later(now, shared.settings.lease)) {
            Ok(Some(claimed)) => {
                let case_id = claimed.case.id.clone();
                // On error the lease is kept: the case is retried once it expires.
                if let Err(error) = activation::run(shared, &mut connection, claimed) {
                    tracing::error!(case_id = %case_id, %error, "activation failed");
                }
            }
            Ok(None) => shared.signal.wait(seen, shared.settings.idle_poll),
            Err(error) => {
                tracing::error!(%error, "cannot claim work");
                shared.signal.wait(seen, shared.settings.idle_poll);
            }
        }
    }
}

fn scheduler_loop(shared: &Shared) {
    let mut connection = match shared.db.connect() {
        Ok(connection) => connection,
        Err(error) => {
            tracing::error!(%error, "scheduler cannot open the database");
            return;
        }
    };
    while !shared.is_shutting_down() {
        let seen = shared.signal.generation();
        let now = Utc::now();
        match scheduler::tick(&mut connection, now, shared.settings.scheduler_batch) {
            Ok(0) => {}
            Ok(fired) => {
                tracing::info!(fired, "wait conditions fired");
                shared.signal.notify();
            }
            Err(error) => tracing::error!(%error, "scheduler tick failed"),
        }
        shared.last_tick_millis.store(now.timestamp_millis(), Ordering::SeqCst);
        shared.signal.wait(seen, shared.settings.scheduler_tick);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::VecDeque;

    use chrono::{DateTime, TimeZone, Utc};
    use clankjob_core::llm::{CompletionRequest, CompletionResponse, LlmError, LlmProvider};
    use tempfile::TempDir;

    use super::*;

    /// A migrated database in a temporary directory, deleted when dropped.
    pub struct TestDb {
        pub db: Db,
        pub files_dir: std::path::PathBuf,
        _dir: TempDir,
    }

    impl TestDb {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = Db::new(dir.path().join("test.db"));
            db.migrate().unwrap();
            Self {
                db,
                files_dir: dir.path().join("files"),
                _dir: dir,
            }
        }

        pub fn connect(&self) -> Connection {
            self.db.connect().unwrap()
        }
    }

    /// A fixed point in time so tests are deterministic.
    pub fn time(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds.saturating_add(1_790_000_000), 0).unwrap()
    }

    /// An LLM that replays scripted responses and records every request.
    #[derive(Default)]
    pub struct ScriptedProvider {
        responses: Mutex<VecDeque<std::result::Result<CompletionResponse, LlmError>>>,
        pub requests: Mutex<Vec<CompletionRequest>>,
        vision: bool,
    }

    impl ScriptedProvider {
        pub fn new<I>(responses: I) -> Arc<Self>
        where
            I: IntoIterator<Item = std::result::Result<CompletionResponse, LlmError>>,
        {
            Arc::new(Self {
                responses: Mutex::new(responses.into_iter().collect()),
                requests: Mutex::default(),
                vision: false,
            })
        }

        /// The same, for a model that can see images.
        pub fn with_vision<I>(responses: I) -> Arc<Self>
        where
            I: IntoIterator<Item = std::result::Result<CompletionResponse, LlmError>>,
        {
            Arc::new(Self {
                responses: Mutex::new(responses.into_iter().collect()),
                requests: Mutex::default(),
                vision: true,
            })
        }

        pub fn request_count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    impl LlmProvider for ScriptedProvider {
        fn default_model(&self) -> &'static str {
            "scripted"
        }

        fn supports_images(&self) -> bool {
            self.vision
        }

        fn complete(&self, request: &CompletionRequest) -> std::result::Result<CompletionResponse, LlmError> {
            self.requests.lock().unwrap().push(request.clone());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(LlmError::Fatal("script exhausted".to_owned())))
        }
    }

    /// An engine using `provider` as its `default` LLM, with instant retries.
    pub fn engine(test_db: &TestDb, provider: Arc<ScriptedProvider>) -> Engine {
        let providers = HashMap::from([("default".to_owned(), provider as Arc<dyn LlmProvider>)]);
        let settings = EngineSettings {
            workers: 1,
            scheduler_tick: Duration::from_millis(20),
            idle_poll: Duration::from_millis(20),
            llm_retry_backoff: Duration::ZERO,
            requeue_delay: Duration::ZERO,
            ..EngineSettings::default()
        };
        Engine::new(
            test_db.db.clone(),
            providers,
            None,
            test_db.files_dir.clone(),
            Channels::default(),
            settings,
        )
    }

    /// The same, with `channel` configured as `discord_joe`, the default channel.
    pub fn engine_with_channels(
        test_db: &TestDb,
        provider: Arc<ScriptedProvider>,
        channel: Arc<dyn clankjob_core::channel::HumanChannel>,
    ) -> Engine {
        let base = engine(test_db, provider);
        let shared = &base.shared;
        let channels = Channels::new(
            std::collections::BTreeMap::from([("discord_joe".to_owned(), channel)]),
            vec!["discord_joe".to_owned()],
            Some("https://clank.example/".to_owned()),
        );
        Engine::new(
            shared.db.clone(),
            shared.providers.clone(),
            None,
            test_db.files_dir.clone(),
            channels,
            shared.settings.clone(),
        )
    }
}
