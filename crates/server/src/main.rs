//! The clankjob server: loads the configuration, opens the database, starts the engine
//! and serves the REST API until it receives `SIGTERM` or `SIGINT` (design §18).
//!
//! Usage: `clankjob [CONFIG]`. The configuration path defaults to `$CLANKJOB_CONFIG`,
//! then `/config/clankjob.toml`. `SIGHUP` reloads prompt templates.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, anyhow};
use clankjob_core::llm::LlmProvider;
use clankjob_engine::{Engine, EngineSettings};
use clankjob_llm_openai::{OpenAiCompatible, OpenAiConfig};
use clankjob_storage::Db;
use secrecy::SecretString;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

mod api;
mod config;

use config::{Config, ProviderKind};

const DEFAULT_CONFIG_PATH: &str = "/config/clankjob.toml";

/// Log as JSON when `CLANKJOB_LOG_FORMAT=json` (containers), as text otherwise.
/// The level comes from `RUST_LOG`, defaulting to `info`.
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if std::env::var("CLANKJOB_LOG_FORMAT").is_ok_and(|format| format == "json") {
        builder.json().init();
    } else {
        builder.init();
    }
}

fn config_path() -> PathBuf {
    std::env::args_os()
        .nth(1)
        .or_else(|| std::env::var_os("CLANKJOB_CONFIG"))
        .map_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH), PathBuf::from)
}

fn build_providers(config: &Config) -> anyhow::Result<HashMap<String, Arc<dyn LlmProvider>>> {
    config
        .llm
        .iter()
        .map(|(name, llm)| {
            let api_key = llm
                .api_key
                .as_ref()
                .map(|key| key.resolve(&config.secrets_dir))
                .transpose()
                .with_context(|| format!("LLM `{name}`"))?;
            let provider: Arc<dyn LlmProvider> = match llm.provider {
                ProviderKind::OpenAiCompatible => Arc::new(OpenAiCompatible::new(OpenAiConfig {
                    base_url: llm.base_url.clone(),
                    api_key,
                    model: llm.model.clone(),
                    timeout: llm.timeout,
                })),
            };
            Ok((name.clone(), provider))
        })
        .collect()
}

fn run() -> anyhow::Result<()> {
    let path = config_path();
    let config = Config::load(&path).with_context(|| format!("loading {}", path.display()))?;
    let tokens: Vec<SecretString> = config
        .api
        .tokens
        .iter()
        .map(|token| token.resolve(&config.secrets_dir))
        .collect::<Result<_, _>>()
        .context("API tokens")?;
    let providers = build_providers(&config)?;

    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating data directory {}", config.data_dir.display()))?;
    let db = Db::new(config.data_dir.join("clankjob.db"));
    db.migrate().context("migrating the database")?;
    // A fresh process owns no work: interrupted activations resume right away (§18.4).
    let cleared = clankjob_storage::queue::clear_leases(&db.connect()?)?;
    tracing::info!(cleared, "leases from the previous run cleared");

    let settings = EngineSettings {
        workers: config.workers,
        ..EngineSettings::default()
    };
    // Rejected prompt files are logged by the engine as they are loaded.
    let engine = Engine::new(db, providers, config.prompts_dir.clone(), settings);
    let workers = engine.start().context("starting engine threads")?;

    let defaults = api::CaseDefaults {
        llm: config.default_llm.clone(),
        profile: config.default_profile.clone(),
        budgets: config.budgets,
    };
    let state = api::AppState::new(engine.clone(), tokens, defaults);
    let server = rouille::Server::new(&config.listen, move |request| api::handle(&state, request))
        .map_err(|error| anyhow!("cannot listen on {}: {error}", config.listen))?;
    tracing::info!(address = %server.server_addr(), "listening");
    let (server_thread, stop_server) = server.stoppable();

    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP]).context("installing signal handlers")?;
    for signal in signals.forever() {
        if signal == SIGHUP {
            let prompts = engine.reload_prompts();
            tracing::info!(errors = prompts.errors().len(), "prompts reloaded on SIGHUP");
            continue;
        }
        tracing::info!(signal, "shutting down");
        break;
    }

    // Stop accepting requests, then let workers finish their current step.
    // A send error only means the server thread already exited.
    let _ = stop_server.send(());
    engine.shutdown();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let _ = server_thread.join();
        for worker in workers {
            let _ = worker.join();
        }
        let _ = done.send(());
    });
    if finished.recv_timeout(config.shutdown_grace).is_err() {
        // Interrupted activations keep their lease and resume after the next start.
        tracing::warn!("shutdown grace period elapsed; exiting with work in progress");
    }
    Ok(())
}

fn main() -> ExitCode {
    init_tracing();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
