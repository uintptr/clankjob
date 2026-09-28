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
use clankjob_core::case::{Case, CaseState, NewCase};
use clankjob_core::human::HumanRequest;
use clankjob_core::ids::{CaseId, HumanRequestId};
use clankjob_core::llm::LlmProvider;
use clankjob_storage::human::Answer;
use clankjob_storage::{Connection, Db, StorageError};

mod activation;
pub mod context;
pub mod prompts;
mod scheduler;
pub mod tools;
pub mod transitions;

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
    /// A prompt template failed to render.
    #[error(transparent)]
    Prompt(#[from] prompts::RenderError),
}

/// Shorthand for results of engine operations.
pub type Result<T> = std::result::Result<T, EngineError>;

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
    /// * `settings` - Tuning knobs
    pub fn new(
        db: Db,
        providers: HashMap<String, Arc<dyn LlmProvider>>,
        prompts_dir: Option<PathBuf>,
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
                settings,
                signal: WorkSignal::default(),
                shutdown: AtomicBool::new(false),
                last_tick_millis: AtomicI64::new(0),
            }),
        }
    }

    /// Start the worker threads and the scheduler thread.
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
    /// Returns [`EngineError::UnknownLlm`] or [`EngineError::UnknownProfile`] if the case
    /// refers to something that is not configured, or [`EngineError::Storage`].
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
        let case = transitions::create_case(connection, new_case, Utc::now())?;
        self.shared.signal.notify();
        Ok(case)
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
        _dir: TempDir,
    }

    impl TestDb {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = Db::new(dir.path().join("test.db"));
            db.migrate().unwrap();
            Self { db, _dir: dir }
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
    }

    impl ScriptedProvider {
        pub fn new<I>(responses: I) -> Arc<Self>
        where
            I: IntoIterator<Item = std::result::Result<CompletionResponse, LlmError>>,
        {
            Arc::new(Self {
                responses: Mutex::new(responses.into_iter().collect()),
                requests: Mutex::default(),
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
        Engine::new(test_db.db.clone(), providers, None, settings)
    }
}
