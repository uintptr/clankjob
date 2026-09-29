//! Tools and guides offered by plugins (design §9), next to the core tools.
//!
//! Every case sees every loaded plugin tool. The set is swapped as a whole when plugins
//! are reloaded.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};

use clankjob_core::llm::ToolSpec;
use clankjob_core::tool::{Guide, PluginTool};

use crate::tools::CORE_TOOL_NAMES;

#[derive(Clone, Default)]
struct ToolSet {
    tools: BTreeMap<String, Arc<dyn PluginTool>>,
    guides: Vec<Guide>,
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
    pub fn replace(&self, tools: Vec<Arc<dyn PluginTool>>, guides: Vec<Guide>) -> Vec<String> {
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

    /// Specs of every plugin tool, by name.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.set
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .tools
            .values()
            .map(|tool| tool.spec().clone())
            .collect()
    }

    /// Every guide.
    #[must_use]
    pub fn guides(&self) -> Vec<Guide> {
        self.set.read().unwrap_or_else(PoisonError::into_inner).guides.clone()
    }
}
