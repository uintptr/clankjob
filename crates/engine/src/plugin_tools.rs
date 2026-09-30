//! Tools and guides offered by plugins (design §9), next to the core tools.
//!
//! A case sees a plugin's tools only once it has loaded the plugin (with `load_plugin`, by
//! reading one of its guides, or by calling one of its tools); until then the system
//! prompt only lists the plugin, so unused plugins cost a line instead of their schemas.
//! The set is swapped as a whole when plugins are reloaded.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError, RwLock};

use clankjob_core::event::{Event, EventBody};
use clankjob_core::llm::ToolSpec;
use clankjob_core::tool::{Guide, PluginCondition, PluginTool};
use serde::Serialize;

use crate::tools::{CORE_TOOL_NAMES, LOAD_PLUGIN, READ_GUIDE};

/// A plugin as listed in the system prompt, for `load_plugin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginEntry {
    /// Plugin id, what `load_plugin` takes.
    pub id: String,
    /// Names of its tools.
    pub tools: Vec<String>,
    /// Kind names of its wait conditions.
    pub conditions: Vec<String>,
    /// The case has loaded it, so its tools are offered.
    pub loaded: bool,
}

/// The entry for `plugin`, added if missing.
fn entry<'a>(
    entries: &'a mut BTreeMap<String, PluginEntry>,
    plugin: &str,
    loaded: &BTreeSet<String>,
) -> &'a mut PluginEntry {
    entries.entry(plugin.to_owned()).or_insert_with(|| PluginEntry {
        id: plugin.to_owned(),
        tools: Vec::new(),
        conditions: Vec::new(),
        loaded: loaded.contains(plugin),
    })
}

#[derive(Clone, Default)]
struct ToolSet {
    tools: BTreeMap<String, Arc<dyn PluginTool>>,
    guides: Vec<Guide>,
    conditions: BTreeMap<String, Arc<dyn PluginCondition>>,
}

/// The plugin tools and guides in use.
#[derive(Default)]
pub struct PluginTools {
    set: RwLock<ToolSet>,
}

impl PluginTools {
    /// Swap in a new set, e.g. after plugins were reloaded.
    ///
    /// A tool whose name is taken by a core tool or an earlier plugin tool is left out,
    /// and so is a guide whose name is already taken.
    ///
    /// # Returns
    ///
    /// Why each left-out tool or guide was refused
    pub fn replace(
        &self,
        tools: Vec<Arc<dyn PluginTool>>,
        guides: Vec<Guide>,
        conditions: Vec<Arc<dyn PluginCondition>>,
    ) -> Vec<String> {
        let mut refused = Vec::new();
        let mut set = ToolSet::default();
        for tool in tools {
            let name = tool.spec().name.clone();
            if CORE_TOOL_NAMES.contains(&name.as_str()) || set.tools.contains_key(&name) {
                refused.push(format!(
                    "tool `{name}` of plugin `{}`: the name is already taken",
                    tool.plugin()
                ));
                continue;
            }
            set.tools.insert(name, tool);
        }
        for guide in guides {
            if set.guides.iter().any(|existing| existing.name == guide.name) {
                refused.push(format!(
                    "guide `{}` of plugin `{}`: the name is already taken",
                    guide.name, guide.plugin
                ));
                continue;
            }
            set.guides.push(guide);
        }
        for condition in conditions {
            let name = condition.name().to_owned();
            if name.starts_with("core.") || set.conditions.contains_key(&name) {
                refused.push(format!(
                    "condition `{name}` of plugin `{}`: the name is already taken",
                    condition.plugin()
                ));
                continue;
            }
            set.conditions.insert(name, condition);
        }
        for reason in &refused {
            tracing::warn!("{reason}");
        }
        *self.set.write().unwrap_or_else(PoisonError::into_inner) = set;
        refused
    }

    /// The tool with this name, if a plugin offers it.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn PluginTool>> {
        self.set
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .tools
            .get(name)
            .map(Arc::clone)
    }

    /// Specs of the tools of the given plugins, by name.
    #[must_use]
    pub fn specs(&self, plugins: &BTreeSet<String>) -> Vec<ToolSpec> {
        self.set
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .tools
            .values()
            .filter(|tool| plugins.contains(tool.plugin()))
            .map(|tool| tool.spec().clone())
            .collect()
    }

    /// The plugins a case has loaded, from its event log: those named by a successful
    /// `load_plugin` or `read_guide`, and those whose tools it called.
    #[must_use]
    pub fn loaded(&self, events: &[Event]) -> BTreeSet<String> {
        let set = self.set.read().unwrap_or_else(PoisonError::into_inner);
        events
            .iter()
            .filter_map(|event| match &event.body {
                EventBody::ToolResult(result) if !result.is_error => Some(result),
                _ => None,
            })
            .filter_map(|result| match result.tool_name.as_str() {
                LOAD_PLUGIN | READ_GUIDE => result.content.get("plugin")?.as_str().map(str::to_owned),
                name => set.tools.get(name).map(|tool| tool.plugin().to_owned()),
            })
            .collect()
    }

    /// Every plugin with tools or wait conditions, by id, for the system prompt.
    ///
    /// # Arguments
    ///
    /// * `loaded` - The plugins the case has loaded, from [`Self::loaded`]
    #[must_use]
    pub fn index(&self, loaded: &BTreeSet<String>) -> Vec<PluginEntry> {
        let set = self.set.read().unwrap_or_else(PoisonError::into_inner);
        let mut entries = BTreeMap::new();
        for (name, tool) in &set.tools {
            entry(&mut entries, tool.plugin(), loaded).tools.push(name.clone());
        }
        for (name, condition) in &set.conditions {
            entry(&mut entries, condition.plugin(), loaded).conditions.push(name.clone());
        }
        entries.into_values().collect()
    }

    /// The wait condition with this kind name, if a plugin offers it.
    #[must_use]
    pub fn condition(&self, name: &str) -> Option<Arc<dyn PluginCondition>> {
        self.set
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .conditions
            .get(name)
            .map(Arc::clone)
    }

    /// Every plugin wait condition, by name.
    #[must_use]
    pub fn conditions(&self) -> Vec<Arc<dyn PluginCondition>> {
        self.set
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .conditions
            .values()
            .map(Arc::clone)
            .collect()
    }

    /// Every guide.
    #[must_use]
    pub fn guides(&self) -> Vec<Guide> {
        self.set.read().unwrap_or_else(PoisonError::into_inner).guides.clone()
    }
}
