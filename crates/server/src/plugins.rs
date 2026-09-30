//! Plugins at runtime: loads the plugin directories, hands the channels to the engine, and
//! loads everything again when a plugin or its settings change (design §9.4).
//!
//! A reload builds a complete new registry (starting and checking every instance) and only
//! then swaps it in, so channels keep working while it runs. The old plugin processes are
//! shut down afterwards.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use clankjob_engine::Engine;
use clankjob_plugin_host::{PluginPaths, PluginRegistry};

/// How often the plugin directories are scanned for changes.
const WATCH_INTERVAL: Duration = Duration::from_secs(3);
/// How long a change must stay put before reloading, so a save in progress isn't loaded.
const SETTLE: Duration = Duration::from_secs(1);

/// One file's identity for change detection: path, modification time and size.
type Stamp = (PathBuf, Option<SystemTime>, u64);

/// Owns the current plugin registry.
pub struct PluginManager {
    paths: PluginPaths,
    default_channels: Option<Vec<String>>,
    engine: Engine,
    current: RwLock<Arc<PluginRegistry>>,
    reloading: Mutex<()>,
    stopped: AtomicBool,
    /// Tools and guides left out because their name was taken.
    conflicts: Mutex<Vec<String>>,
}

impl PluginManager {
    /// Create a manager with nothing loaded; call [`PluginManager::reload`] to load.
    ///
    /// # Arguments
    ///
    /// * `paths` - The plugin directories, the settings directory and the secrets
    /// * `default_channels` - `default_human_channels` from the config
    /// * `engine` - Receives the loaded channels
    #[must_use]
    pub fn new(paths: PluginPaths, default_channels: Option<Vec<String>>, engine: Engine) -> Self {
        Self {
            paths,
            default_channels,
            engine,
            current: RwLock::new(Arc::new(PluginRegistry::default())),
            reloading: Mutex::new(()),
            stopped: AtomicBool::new(false),
            conflicts: Mutex::new(Vec::new()),
        }
    }

    /// Where plugins and their settings are read from.
    #[must_use]
    pub fn paths(&self) -> &PluginPaths {
        &self.paths
    }

    /// Tools and guides left out at the last load because their name was taken.
    #[must_use]
    pub fn conflicts(&self) -> Vec<String> {
        self.conflicts.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The registry in use.
    #[must_use]
    pub fn current(&self) -> Arc<PluginRegistry> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Load every plugin again, check each instance, and hand the channels to the engine.
    /// Blocks until done; concurrent calls wait for each other.
    ///
    /// # Returns
    ///
    /// The new registry
    pub fn reload(&self) -> Arc<PluginRegistry> {
        let _one_at_a_time = self.reloading.lock().unwrap_or_else(PoisonError::into_inner);
        if self.stopped.load(Ordering::SeqCst) {
            return self.current();
        }
        let registry = Arc::new(if self.paths.dirs.is_empty() {
            PluginRegistry::default()
        } else {
            clankjob_plugin_host::load(&self.paths)
        });
        let channels = registry.channels();
        let names: Vec<String> = channels.keys().cloned().collect();
        // Omitted: the web client only; the owner turns a channel on per case.
        let default = match &self.default_channels {
            None => Vec::new(),
            Some(wanted) => wanted
                .iter()
                .filter(|name| {
                    let loaded = names.contains(name);
                    if !loaded {
                        tracing::warn!(channel = %name, "default human channel is not loaded; skipped");
                    }
                    loaded
                })
                .cloned()
                .collect(),
        };
        tracing::info!(
            channels = ?names,
            ?default,
            tools = registry.tools().len(),
            guides = registry.guides().len(),
            errors = registry.errors().len(),
            "plugins loaded"
        );
        self.engine.channels().replace(channels, default);
        let conflicts = self
            .engine
            .plugin_tools()
            .replace(registry.tools(), registry.guides(), registry.conditions());
        *self.conflicts.lock().unwrap_or_else(PoisonError::into_inner) = conflicts;
        let previous = std::mem::replace(
            &mut *self.current.write().unwrap_or_else(PoisonError::into_inner),
            Arc::clone(&registry),
        );
        previous.shutdown();
        registry
    }

    /// Reload whenever a file in a plugin directory or the settings directory changes.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the thread cannot be created.
    pub fn watch(self: &Arc<Self>) -> std::io::Result<Option<JoinHandle<()>>> {
        if self.paths.dirs.is_empty() {
            return Ok(None);
        }
        let manager = Arc::clone(self);
        thread::Builder::new()
            .name("plugin-watch".to_owned())
            .spawn(move || {
                let paths = &manager.paths;
                let mut last = fingerprint(paths);
                while !manager.stopped.load(Ordering::SeqCst) {
                    thread::sleep(WATCH_INTERVAL);
                    let seen = fingerprint(paths);
                    if seen == last {
                        continue;
                    }
                    thread::sleep(SETTLE);
                    let settled = fingerprint(paths);
                    if settled != seen {
                        // Still being written; look again next time.
                        continue;
                    }
                    tracing::info!("plugins or their settings changed; reloading");
                    manager.reload();
                    last = settled;
                }
            })
            .map(Some)
    }

    /// Stop reloading and stop (re)starting plugin processes, without waiting for calls in
    /// progress: the first step of shutting down, before the engine's threads are joined.
    pub fn retire(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.current().retire();
    }

    /// Stop watching and shut down every plugin process.
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _one_at_a_time = self.reloading.lock().unwrap_or_else(PoisonError::into_inner);
        self.current().shutdown();
    }
}

/// Whether a file in a plugin directory is worth watching: not hidden, not a cache,
/// not a log.
fn watched(path: &Path) -> bool {
    let name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    !(name.starts_with('.') || name == "__pycache__" || name.ends_with(".pyc") || name.ends_with(".log"))
}

/// What the plugins look like: every plugin directory and the files directly inside it,
/// and every file of the settings directory, with their modification times and sizes.
fn fingerprint(paths: &PluginPaths) -> Vec<Stamp> {
    let stamp = |path: PathBuf| -> Stamp {
        let metadata = std::fs::metadata(&path).ok();
        let modified = metadata.as_ref().and_then(|metadata| metadata.modified().ok());
        (path, modified, metadata.map_or(0, |metadata| metadata.len()))
    };
    let entries = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(std::result::Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| watched(path))
                    .collect()
            })
            .unwrap_or_default()
    };
    let files = |dir: &Path| entries(dir).into_iter().filter(|path| path.is_file()).map(stamp);
    let mut stamps: Vec<Stamp> = paths.config_dir.as_deref().map(|dir| files(dir).collect()).unwrap_or_default();
    for plugin in paths.dirs.iter().flat_map(|dir| entries(dir)).filter(|path| path.is_dir()) {
        stamps.extend(files(&plugin));
        // By name only: a directory's own time changes when a cache appears inside it.
        stamps.push((plugin, None, 0));
    }
    stamps.sort();
    stamps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_sees_config_changes_but_not_caches_or_logs() {
        // Arrange
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/chat");
        let config_dir = root.path().join("config");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::create_dir(&config_dir).unwrap();
        std::fs::write(plugin.join("config.toml"), "a = 1\n").unwrap();
        let paths = PluginPaths {
            dirs: vec![root.path().join("plugins")],
            config_dir: Some(config_dir.clone()),
            secrets_dir: PathBuf::new(),
        };
        let before = fingerprint(&paths);

        // Act
        std::fs::create_dir(plugin.join("__pycache__")).unwrap();
        std::fs::write(plugin.join("calls.log"), "noise").unwrap();
        let after_noise = fingerprint(&paths);
        std::fs::write(plugin.join("config.toml"), "a = 22\n").unwrap();
        let after_edit = fingerprint(&paths);
        std::fs::write(config_dir.join("chat.toml"), "a = 3\n").unwrap();
        let after_settings = fingerprint(&paths);

        // Assert
        assert_eq!(before.len(), 2);
        assert_eq!(after_noise, before);
        assert_ne!(after_edit, before);
        assert_ne!(after_settings, after_edit);
    }
}
