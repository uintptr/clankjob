//! Prompt templates (design §7.4).
//!
//! Every text sent to the LLM comes from a template. Built-in defaults are compiled into
//! the binary; a file in the prompts directory replaces the default of the same name.
//! Templates are validated when loaded by rendering them against sample data, so a typo
//! is reported at load time instead of breaking a case later.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use clankjob_core::case::{Budgets, CaseNote, Instruction, Usage};
use clankjob_core::event::{InstructionChange, WakeReason};
use clankjob_core::file::FileKind;
use clankjob_core::ids::{CaseId, FileId, HumanRequestId, InstructionId, WaitConditionId};
use clankjob_core::llm::ToolSpec;
use minijinja::{Environment, UndefinedBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Template for the platform rules at the start of the system prompt.
pub const SYSTEM: &str = "system";
/// Template for the case summary (goal, notes, budgets) in the system prompt.
pub const CASE_HEADER: &str = "case_header";
/// Template that turns a wake reason into a user message.
pub const WAKE: &str = "wake";
/// Template sent when the LLM answers without calling a tool.
pub const NUDGE: &str = "nudge";
/// Template for the owner's instructions, always in the system prompt.
pub const INSTRUCTIONS: &str = "instructions";
/// Template listing the files added to a case, in the system prompt.
pub const FILES: &str = "files";

/// Template presenting the owner's own prompt (`user_prompt.md`), in the system prompt.
pub const USER_PROMPT: &str = "user_prompt";

/// Template listing the guides plugins offer, in the system prompt.
pub const GUIDES: &str = "guides";

/// Template listing the plugins a case can load with `load_plugin`, in the system prompt.
pub const PLUGINS: &str = "plugins";

/// Name prefix of profile templates, e.g. `profiles/quotes`.
const PROFILE_PREFIX: &str = "profiles/";

/// Built-in templates. `include_str!` embeds each file in the binary at compile time.
const BUILTINS: [(&str, &str); 9] = [
    (SYSTEM, include_str!("../prompts/system.md.j2")),
    (CASE_HEADER, include_str!("../prompts/case_header.md.j2")),
    (INSTRUCTIONS, include_str!("../prompts/instructions.md.j2")),
    (FILES, include_str!("../prompts/files.md.j2")),
    (GUIDES, include_str!("../prompts/guides.md.j2")),
    (PLUGINS, include_str!("../prompts/plugins.md.j2")),
    (USER_PROMPT, include_str!("../prompts/user_prompt.md.j2")),
    (WAKE, include_str!("../prompts/wake.md.j2")),
    (NUDGE, include_str!("../prompts/nudge.md.j2")),
];

/// Where a template's content came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSource {
    /// The default compiled into the binary.
    Builtin,
    /// A file in the prompts directory.
    File,
}

/// An effective template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prompt {
    /// Template name, e.g. `system` or `profiles/quotes`.
    pub name: String,
    /// Where it came from.
    pub source: PromptSource,
    /// SHA-256 of the content, hex encoded.
    pub hash: String,
    /// Template source.
    pub content: String,
}

/// A template file that could not be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptLoadError {
    /// Template name.
    pub name: String,
    /// What was wrong with it.
    pub message: String,
}

/// Error rendering a template.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("prompt `{name}` failed to render: {message}")]
pub struct RenderError {
    name: String,
    message: String,
}

/// The case as seen by templates.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaseView<'a> {
    /// Case title.
    pub title: &'a str,
    /// Case goal.
    pub goal: &'a str,
    /// Case owner.
    pub owner: Option<&'a str>,
    /// Creation time, RFC 3339.
    pub created_at: String,
}

/// Variables available to every template.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PromptContext<'a> {
    /// Current time, RFC 3339.
    pub now: String,
    /// The case.
    pub case: CaseView<'a>,
    /// Its budgets.
    pub budgets: &'a Budgets,
    /// Its usage so far.
    pub usage: &'a Usage,
    /// Its activations in the last 24 hours, the current one included.
    pub activations_today: u32,
    /// Its notes.
    pub notes: &'a [CaseNote],
    /// Tools available to the LLM.
    pub tools: &'a [ToolSpec],
    /// The owner's instructions for the case.
    pub instructions: &'a [Instruction],
    /// Files added to the case, as presented to the agent.
    pub files: &'a [crate::files::FileView],
    /// Guides plugins offer, read with `read_guide`.
    pub guides: &'a [clankjob_core::tool::Guide],
    /// Plugins the case can load, or has loaded, with `load_plugin`.
    pub plugins: &'a [crate::plugin_tools::PluginEntry],
    /// The owner's own prompt, when they wrote one.
    pub user_prompt: Option<&'a str>,
    /// Why the case woke up; only set when rendering the `wake` template.
    pub wake: Option<&'a WakeReason>,
}

/// Which variables a template is rendered with, to validate it against the right data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateRole {
    /// Rendered with `wake` unset.
    Case,
    /// Rendered once per wake reason.
    Wake,
}

fn role_of(name: &str) -> TemplateRole {
    if name == WAKE {
        TemplateRole::Wake
    } else {
        TemplateRole::Case
    }
}

fn environment() -> Environment<'static> {
    let mut environment = Environment::new();
    // Strict: a misspelled variable is an error instead of silently rendering as empty.
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    // Drop the newline after a `{% ... %}` tag and the indentation before it, so block
    // tags can sit on their own lines without leaving blank lines in the output.
    environment.set_trim_blocks(true);
    environment.set_lstrip_blocks(true);
    environment
}

fn render_source(name: &str, source: &str, context: &PromptContext<'_>) -> Result<String, RenderError> {
    environment()
        .render_str(source, context)
        .map(|text| text.trim().to_owned())
        .map_err(|error| RenderError {
            name: name.to_owned(),
            message: format!("{error:#}"),
        })
}

/// SHA-256 of some bytes, hex encoded.
pub(crate) fn sha256_hex<B>(content: B) -> String
where
    B: AsRef<[u8]>,
{
    let digest = Sha256::digest(content.as_ref());
    digest.iter().fold(String::with_capacity(64), |mut hex, byte| {
        // Writing to a String cannot fail, so the result is ignored.
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// Sample files, guides and instructions for [`validate`].
fn sample_material() -> (
    [crate::files::FileView; 1],
    [clankjob_core::tool::Guide; 1],
    [Instruction; 1],
) {
    let files = [crate::files::FileView {
        name: "notes.md".to_owned(),
        kind: FileKind::Text,
        size: "1 KB".to_owned(),
        pages: Some(1),
        access: crate::files::Access::ReadFile,
        note: None,
    }];
    let guides = [clankjob_core::tool::Guide {
        plugin: "plugin".to_owned(),
        name: "guide".to_owned(),
        description: "When to use it.".to_owned(),
        content: String::new(),
    }];
    let instructions = [Instruction {
        id: InstructionId::generate(),
        case_id: CaseId::generate(),
        name: "tone.md".to_owned(),
        content: "Be polite.".to_owned(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }];
    (files, guides, instructions)
}

/// One of every wake reason, for [`validate`].
fn sample_wake_reasons() -> Vec<WakeReason> {
    let condition_id = WaitConditionId::generate();
    vec![
        WakeReason::Created,
        WakeReason::ApprovalDecided {
            request_id: HumanRequestId::generate(),
            tool: "send_email".to_owned(),
            decision: clankjob_core::human::Decision::Approve,
            comment: Some("ok".to_owned()),
            edited_args: Some(serde_json::json!({"to": "a@b.c"})),
            via: Some("web".to_owned()),
        },
        WakeReason::ApprovalDecided {
            request_id: HumanRequestId::generate(),
            tool: "send_email".to_owned(),
            decision: clankjob_core::human::Decision::Reject,
            comment: None,
            edited_args: None,
            via: None,
        },
        WakeReason::ApprovedCallFinished {
            request_id: HumanRequestId::generate(),
            tool: "send_email".to_owned(),
            result: serde_json::json!({"message_id": "<1@x>"}),
            is_error: false,
        },
        WakeReason::HumanMessage {
            text: "Hello".to_owned(),
        },
        WakeReason::HumanAnswer {
            request_id: HumanRequestId::generate(),
            question: "Proceed?".to_owned(),
            answer: "Yes".to_owned(),
            via: Some("web".to_owned()),
        },
        WakeReason::ConditionFired {
            condition_id: condition_id.clone(),
            kind: "core.timer".to_owned(),
            details: vec![serde_json::json!({"detail": 1})],
        },
        WakeReason::TimedOut {
            condition_id,
            kind: "core.timer".to_owned(),
        },
        WakeReason::Manual,
        WakeReason::InstructionsChanged {
            instruction_id: InstructionId::generate(),
            name: "tone.md".to_owned(),
            change: InstructionChange::Updated,
        },
        WakeReason::FileAdded {
            file_id: FileId::generate(),
            name: "panel.jpg".to_owned(),
            kind: FileKind::Image,
        },
    ]
}

/// Render a template against sample data covering every variable it may use.
fn validate(name: &str, source: &str) -> Result<(), RenderError> {
    let budgets = Budgets::default();
    let usage = Usage {
        activations: 1,
        input_tokens: 0,
        output_tokens: 0,
    };
    let notes = [CaseNote {
        key: "key".to_owned(),
        value: "value".to_owned(),
        updated_at: chrono::Utc::now(),
    }];
    let tools = [ToolSpec {
        name: "tool".to_owned(),
        description: "A tool.".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let (files, guides, instructions) = sample_material();
    let plugins = [crate::plugin_tools::PluginEntry {
        id: "plugin".to_owned(),
        tools: vec!["tool".to_owned()],
        conditions: vec!["plugin.condition".to_owned()],
        loaded: true,
    }];
    let base = PromptContext {
        now: "2026-01-01T00:00:00Z".to_owned(),
        case: CaseView {
            title: "Title",
            goal: "Goal",
            owner: None,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        },
        budgets: &budgets,
        usage: &usage,
        activations_today: 1,
        notes: &notes,
        tools: &tools,
        instructions: &instructions,
        files: &files,
        guides: &guides,
        plugins: &plugins,
        user_prompt: Some("Sign emails as Brad."),
        wake: None,
    };
    match role_of(name) {
        TemplateRole::Case => render_source(name, source, &base).map(drop),
        TemplateRole::Wake => {
            let reasons = sample_wake_reasons();
            reasons.iter().try_for_each(|reason| {
                render_source(
                    name,
                    source,
                    &PromptContext {
                        wake: Some(reason),
                        ..base.clone()
                    },
                )
                .map(drop)
            })
        }
    }
}

/// The effective set of templates, plus problems found while loading them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSet {
    templates: BTreeMap<String, Prompt>,
    errors: Vec<PromptLoadError>,
}

impl PromptSet {
    /// Only the built-in templates.
    #[must_use]
    pub fn builtin() -> Self {
        let templates = BUILTINS
            .iter()
            .map(|(name, content)| {
                let prompt = Prompt {
                    name: (*name).to_owned(),
                    source: PromptSource::Builtin,
                    hash: sha256_hex(content),
                    content: (*content).to_owned(),
                };
                (prompt.name.clone(), prompt)
            })
            .collect();
        Self {
            templates,
            errors: Vec::new(),
        }
    }

    /// Load templates from a prompts directory on top of the built-ins.
    ///
    /// A file that cannot be read or does not validate is recorded in [`PromptSet::errors`]
    /// and replaced by the previous version of that template, or by the built-in default.
    /// Loading never fails as a whole.
    ///
    /// # Arguments
    ///
    /// * `dir` - The prompts directory; a missing directory means "built-ins only"
    /// * `previous` - The set in use before a reload, used as a fallback
    ///
    /// # Returns
    ///
    /// The effective prompt set
    pub fn load<P>(dir: P, previous: Option<&PromptSet>) -> Self
    where
        P: AsRef<Path>,
    {
        let dir = dir.as_ref();
        let mut set = Self::builtin();
        for (name, _) in BUILTINS {
            set.load_file(name, &dir.join(format!("{name}.md")), previous);
        }
        let profiles_dir = dir.join("profiles");
        if let Ok(entries) = fs::read_dir(&profiles_dir) {
            for path in entries.filter_map(|entry| entry.ok().map(|entry| entry.path())) {
                if let (Some(stem), Some("md")) = (
                    path.file_stem().and_then(|stem| stem.to_str()),
                    path.extension().and_then(|ext| ext.to_str()),
                ) {
                    set.load_file(&format!("{PROFILE_PREFIX}{stem}"), &path, previous);
                }
            }
        }
        set
    }

    /// Replace one template with the file at `path`, if it exists and is valid.
    fn load_file(&mut self, name: &str, path: &Path, previous: Option<&PromptSet>) {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                self.record_error(name, format!("cannot read {}: {error}", path.display()), previous);
                return;
            }
        };
        match validate(name, &content) {
            Ok(()) => {
                let prompt = Prompt {
                    name: name.to_owned(),
                    source: PromptSource::File,
                    hash: sha256_hex(&content),
                    content,
                };
                self.templates.insert(name.to_owned(), prompt);
            }
            Err(error) => self.record_error(name, error.message, previous),
        }
    }

    fn record_error(&mut self, name: &str, message: String, previous: Option<&PromptSet>) {
        tracing::error!(prompt = name, %message, "prompt template rejected");
        if let Some(prompt) = previous.and_then(|previous| previous.templates.get(name)) {
            self.templates.insert(name.to_owned(), prompt.clone());
        }
        self.errors.push(PromptLoadError {
            name: name.to_owned(),
            message,
        });
    }

    /// Render a template.
    ///
    /// # Arguments
    ///
    /// * `name` - Template name, e.g. [`SYSTEM`]
    /// * `context` - Variables
    ///
    /// # Returns
    ///
    /// The rendered text, trimmed
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] if the template does not exist or fails to render.
    pub fn render(&self, name: &str, context: &PromptContext<'_>) -> Result<String, RenderError> {
        let prompt = self.templates.get(name).ok_or_else(|| RenderError {
            name: name.to_owned(),
            message: "no such template".to_owned(),
        })?;
        render_source(name, &prompt.content, context)
    }

    /// Render a profile by its short name (`quotes` for `profiles/quotes`).
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] if the profile does not exist or fails to render.
    pub fn render_profile(&self, profile: &str, context: &PromptContext<'_>) -> Result<String, RenderError> {
        self.render(&format!("{PROFILE_PREFIX}{profile}"), context)
    }

    /// Whether a profile with this short name exists.
    #[must_use]
    pub fn has_profile(&self, profile: &str) -> bool {
        self.templates.contains_key(&format!("{PROFILE_PREFIX}{profile}"))
    }

    /// One effective template by full name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Prompt> {
        self.templates.get(name)
    }

    /// Every effective template, sorted by name.
    pub fn prompts(&self) -> impl Iterator<Item = &Prompt> {
        self.templates.values()
    }

    /// Problems found while loading.
    #[must_use]
    pub fn errors(&self) -> &[PromptLoadError] {
        &self.errors
    }

    /// Name → content hash of every effective template (recorded per activation).
    #[must_use]
    pub fn hashes(&self) -> BTreeMap<String, String> {
        self.templates
            .iter()
            .map(|(name, prompt)| (name.clone(), prompt.hash.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, relative: &str, content: &str) {
        let path = dir.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn context<'a>(budgets: &'a Budgets, usage: &'a Usage) -> PromptContext<'a> {
        PromptContext {
            now: "2026-09-28T12:00:00Z".to_owned(),
            case: CaseView {
                title: "Electrician quote",
                goal: "Get a quote",
                owner: Some("joe"),
                created_at: "2026-09-28T10:00:00Z".to_owned(),
            },
            budgets,
            usage,
            activations_today: 1,
            notes: &[],
            tools: &[],
            instructions: &[],
            files: &[],
            guides: &[],
            plugins: &[],
            user_prompt: None,
            wake: None,
        }
    }

    #[test]
    fn builtin_templates_are_valid() {
        for (name, content) in BUILTINS {
            validate(name, content).unwrap();
        }
    }

    #[test]
    fn case_header_explains_a_case_without_a_goal() {
        let (budgets, usage) = (Budgets::default(), Usage::default());
        let mut context = context(&budgets, &usage);
        let set = PromptSet::builtin();

        let with_goal = set.render(CASE_HEADER, &context).unwrap();
        context.case.goal = "";
        let without = set.render(CASE_HEADER, &context).unwrap();

        assert!(with_goal.contains("Get a quote"));
        assert!(without.contains("created this case with only its title"), "{without}");
    }

    #[test]
    fn missing_directory_gives_builtins_without_errors() {
        let set = PromptSet::load("/nonexistent/prompts", None);

        assert_eq!(set, PromptSet::builtin());
    }

    #[test]
    fn valid_file_overrides_builtin_and_profiles_are_loaded() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "nudge.md", "Call a tool, please.");
        write(dir.path(), "profiles/quotes.md", "Negotiate for {{ case.owner }}.");
        let (budgets, usage) = (Budgets::default(), Usage::default());

        // Act
        let set = PromptSet::load(dir.path(), None);

        // Assert
        assert_eq!(set.errors(), []);
        assert_eq!(set.get(NUDGE).unwrap().source, PromptSource::File);
        assert_eq!(
            set.render(NUDGE, &context(&budgets, &usage)).unwrap(),
            "Call a tool, please."
        );
        assert!(set.has_profile("quotes"));
        assert_eq!(
            set.render_profile("quotes", &context(&budgets, &usage)).unwrap(),
            "Negotiate for joe."
        );
    }

    #[test]
    fn invalid_file_falls_back_to_previous_version() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "nudge.md", "Version one.");
        let previous = PromptSet::load(dir.path(), None);
        write(dir.path(), "nudge.md", "Hello {{ misspelled_variable }}");

        // Act
        let set = PromptSet::load(dir.path(), Some(&previous));

        // Assert
        assert_eq!(set.errors().len(), 1);
        assert_eq!(set.errors()[0].name, NUDGE);
        assert_eq!(set.get(NUDGE).unwrap().content, "Version one.");
    }

    #[test]
    fn invalid_file_without_previous_falls_back_to_builtin() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "wake.md",
            "{% if wake.reason == 'manual' %}{{ wake.nope }}{% endif %}",
        );

        let set = PromptSet::load(dir.path(), None);

        assert_eq!(set.errors().len(), 1);
        assert_eq!(set.get(WAKE).unwrap().source, PromptSource::Builtin);
    }

    #[test]
    fn wake_template_renders_each_reason() {
        let set = PromptSet::builtin();
        let (budgets, usage) = (Budgets::default(), Usage::default());
        let reason = WakeReason::HumanMessage {
            text: "Bob called".to_owned(),
        };

        let text = set
            .render(
                WAKE,
                &PromptContext {
                    wake: Some(&reason),
                    ..context(&budgets, &usage)
                },
            )
            .unwrap();

        assert!(text.starts_with("The owner sent you a message:\n\nBob called"));
    }

    #[test]
    fn case_header_lists_notes() {
        let set = PromptSet::builtin();
        let (budgets, usage) = (Budgets::default(), Usage::default());
        let notes = [CaseNote {
            key: "price".to_owned(),
            value: "1450".to_owned(),
            updated_at: chrono::Utc::now(),
        }];

        let text = set
            .render(
                CASE_HEADER,
                &PromptContext {
                    notes: &notes,
                    ..context(&budgets, &usage)
                },
            )
            .unwrap();

        assert!(text.contains("- price: 1450"));
        assert!(text.contains("- Owner: joe"));
    }

    #[test]
    fn files_template_lists_files_without_their_content() {
        let set = PromptSet::builtin();
        let (budgets, usage) = (Budgets::default(), Usage::default());
        let view = |name: &str, kind, access, pages| crate::files::FileView {
            name: name.to_owned(),
            kind,
            size: "2 KB".to_owned(),
            pages,
            access,
            note: None,
        };
        let files = [
            view("contract.pdf", FileKind::Pdf, crate::files::Access::ReadFile, Some(1)),
            view("panel.jpg", FileKind::Image, crate::files::Access::ViewImage, None),
        ];

        let text = set
            .render(
                FILES,
                &PromptContext {
                    files: &files,
                    ..context(&budgets, &usage)
                },
            )
            .unwrap();

        assert!(text.starts_with("## Files"));
        assert!(text.contains("- `contract.pdf`: pdf, 2 KB, 1 page. Read it with `read_file`.\n"));
        assert!(text.ends_with("- `panel.jpg`: image, 2 KB. Look at it with `view_image`."));
    }

    #[test]
    fn instructions_template_includes_every_instruction_in_full() {
        let set = PromptSet::builtin();
        let (budgets, usage) = (Budgets::default(), Usage::default());
        let instruction = |name: &str, content: &str| Instruction {
            id: InstructionId::generate(),
            case_id: CaseId::generate(),
            name: name.to_owned(),
            content: content.to_owned(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let instructions = [instruction("tone.md", "Be polite.\n"), instruction("budget.md", "Max $1,500.")];

        let text = set
            .render(
                INSTRUCTIONS,
                &PromptContext {
                    instructions: &instructions,
                    ..context(&budgets, &usage)
                },
            )
            .unwrap();

        assert!(text.starts_with("## Instructions from the owner"));
        assert!(text.contains("### tone.md\n\nBe polite.\n\n### budget.md\n\nMax $1,500."));
    }

    #[test]
    fn hashes_change_with_content() {
        assert_ne!(sha256_hex("a"), sha256_hex("b"));
        assert_eq!(sha256_hex("a").len(), 64);
    }
}
