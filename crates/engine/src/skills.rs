//! Skills (design §9.10): what a case learned, saved with `save_skill` for later cases.
//!
//! A skill is read like a plugin guide, with `read_guide`, and may carry scripts that a
//! tool with a skill argument (the shell's `run_command`) copies to the sandbox. Nothing a
//! case saves is used before the owner approves it, since a skill reaches every case: an
//! instruction slipped into one email must not spread through it.

use chrono::{DateTime, Utc};
use clankjob_core::case::Case;
use clankjob_core::skill::{SkillDraft, SkillFile};
use clankjob_core::tool::Guide;
use clankjob_storage::skills::Saver;
use clankjob_storage::{self as storage, Connection};
use serde::Serialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};

use crate::Result;
use crate::plugin_tools::PluginTools;

/// Longest skill name, in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// Longest description, in characters. Every case's system prompt lists it.
pub const MAX_DESCRIPTION_CHARS: usize = 300;
/// Longest instructions, in characters.
pub const MAX_CONTENT_CHARS: usize = 20_000;
/// Most files one skill holds.
pub const MAX_FILES: usize = 20;
/// Longest file name, in characters.
const MAX_FILE_NAME_CHARS: usize = 100;
/// Longest total of a skill's files, in characters.
pub const MAX_FILES_CHARS: usize = 200_000;

/// A skill name made canonical: trimmed and lowercased; 1 to 64 letters, digits and
/// dashes, starting with a letter or digit.
///
/// # Errors
///
/// Returns why the name is not valid.
pub fn clean_name(name: &str) -> std::result::Result<String, String> {
    let name = name.trim().to_lowercase();
    let valid = name.len() <= MAX_NAME_CHARS
        && name.starts_with(|first: char| first.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-');
    if valid {
        Ok(name)
    } else {
        Err(format!(
            "skill name `{name}` must be 1 to {MAX_NAME_CHARS} lowercase letters, digits or dashes, e.g. `weather-forecast`"
        ))
    }
}

fn check_text(what: &str, text: &str, max: usize) -> std::result::Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if text.contains('\0') {
        return Err(format!("{what} contains a NUL byte"));
    }
    let chars = text.chars().count();
    if chars > max {
        return Err(format!("{what} is {chars} characters; at most {max}"));
    }
    Ok(())
}

fn check_file_name(name: &str) -> std::result::Result<(), String> {
    let valid = name.len() <= MAX_FILE_NAME_CHARS
        && name.starts_with(|first: char| first.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "file name `{name}` must be 1 to {MAX_FILE_NAME_CHARS} letters, digits, `.`, `_` or `-`, starting with a letter or digit"
        ))
    }
}

/// A skill's content checked against the limits, with its description and instructions
/// trimmed.
///
/// # Errors
///
/// Returns why it cannot be saved.
pub fn clean(draft: SkillDraft) -> std::result::Result<SkillDraft, String> {
    let description = draft.description.trim().to_owned();
    let content = draft.content.trim().to_owned();
    check_text("the description", &description, MAX_DESCRIPTION_CHARS)?;
    check_text("the instructions", &content, MAX_CONTENT_CHARS)?;
    if draft.files.len() > MAX_FILES {
        return Err(format!("a skill holds at most {MAX_FILES} files"));
    }
    let mut total = 0_usize;
    for (index, file) in draft.files.iter().enumerate() {
        check_file_name(&file.name)?;
        if draft.files.iter().take(index).any(|earlier| earlier.name == file.name) {
            return Err(format!("two files are named `{}`", file.name));
        }
        check_text(&format!("file `{}`", file.name), &file.content, MAX_FILES_CHARS)?;
        total = total.saturating_add(file.content.chars().count());
    }
    if total > MAX_FILES_CHARS {
        return Err(format!("the files total {total} characters; at most {MAX_FILES_CHARS}"));
    }
    Ok(SkillDraft {
        description,
        content,
        files: draft.files,
    })
}

/// A skill to save, checked: its canonical name and cleaned content, refused when a plugin
/// guide has that name.
///
/// # Errors
///
/// Returns why it cannot be saved.
pub fn prepare(name: &str, draft: SkillDraft, guides: &[Guide]) -> std::result::Result<(String, SkillDraft), String> {
    let name = clean_name(name)?;
    if let Some(guide) = guides.iter().find(|guide| guide.name.eq_ignore_ascii_case(&name)) {
        return Err(format!(
            "`{name}` is the name of a guide of plugin `{}`; choose another",
            guide.plugin
        ));
    }
    Ok((name, clean(draft)?))
}

/// What the owner is asked to approve.
///
/// # Errors
///
/// Returns a storage error.
pub(crate) fn approval_summary(connection: &Connection, name: &str, draft: &SkillDraft) -> Result<String> {
    let current = storage::skills::get_version(connection, name, None)?;
    let what = match current {
        Some(current) => format!(
            "Update the skill `{name}` to version {}",
            current.version.saturating_add(1)
        ),
        None => format!("Save a new skill `{name}`"),
    };
    let files = if draft.files.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = draft.files.iter().map(|file| file.name.as_str()).collect();
        format!(", with {}", names.join(", "))
    };
    Ok(format!("{what}{files}: {}", draft.description))
}

/// Save a checked skill as a new version.
///
/// # Returns
///
/// The result for the LLM
///
/// # Errors
///
/// Returns a storage error.
pub(crate) fn save(
    connection: &Connection,
    name: &str,
    draft: &SkillDraft,
    saver: Saver<'_>,
    now: DateTime<Utc>,
) -> Result<Value> {
    let version = storage::skills::save_version(connection, name, draft, saver, now)?;
    let enabled = storage::skills::get_enabled(connection, name)?.is_some();
    let note = if enabled {
        "Every case sees it under Guides from its next turn."
    } else {
        "The owner has disabled this skill, so cases do not see it."
    };
    Ok(json!({ "status": "saved", "skill": name, "version": version, "note": note }))
}

/// `read_guide` for a skill: its instructions, its file names and how to run them.
/// Counts as a use of the skill by the case.
///
/// # Returns
///
/// The result for the LLM, or `None` when no enabled skill has that name
///
/// # Errors
///
/// Returns a storage error.
pub(crate) fn read(
    connection: &Connection,
    case: &Case,
    wanted: &str,
    plugins: &PluginTools,
    now: DateTime<Utc>,
) -> Result<Option<Value>> {
    let Some(skill) = storage::skills::get_enabled(connection, &wanted.trim().to_lowercase())? else {
        return Ok(None);
    };
    storage::skills::record_use(connection, &skill.skill, &case.id, now)?;
    let files: Vec<&str> = skill.draft.files.iter().map(|file: &SkillFile| file.name.as_str()).collect();
    let mut result = json!({
        "guide": skill.skill,
        "skill": true,
        "version": skill.version,
        "content": skill.draft.content,
        "files": files,
    });
    if files.is_empty() {
        return Ok(Some(result));
    }
    let run = match plugins.skill_runner() {
        Some(runner) => {
            if let Some(object) = result.as_object_mut() {
                // Reading the skill loads the plugin that runs its scripts.
                object.insert("plugin".to_owned(), Value::String(runner.plugin));
            }
            format!(
                "Its files are copied fresh to `skills/{}/` when you call `{}` with `{}`: `{}`.",
                skill.skill, runner.tool, runner.argument, skill.skill
            )
        }
        None => "Its scripts need a tool that runs skills, and no plugin offers one now.".to_owned(),
    };
    if let Some(object) = result.as_object_mut() {
        object.insert("run".to_owned(), Value::String(run));
    }
    Ok(Some(result))
}

/// Unchanged lines shown around each change in a diff.
const DIFF_CONTEXT: usize = 3;

/// One line of a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    /// `=` unchanged, `-` removed, `+` added, `…` unchanged lines left out.
    pub op: &'static str,
    /// The line, without its newline.
    pub text: String,
}

/// How a part of a skill changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PartStatus {
    /// Only in the new version.
    Added,
    /// Only in the old version.
    Removed,
    /// In both, different.
    Changed,
    /// In both, the same.
    Same,
}

/// The diff of one part of a skill: its description, its instructions, or a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffPart {
    /// `description`, `instructions`, or a file name.
    pub part: String,
    /// How it changed.
    pub status: PartStatus,
    /// The changed lines with some context; empty when the same.
    pub lines: Vec<DiffLine>,
}

/// Line diff of two texts, changes with [`DIFF_CONTEXT`] lines around them.
fn diff_text(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    let mut lines = Vec::new();
    for (index, group) in diff.grouped_ops(DIFF_CONTEXT).iter().enumerate() {
        if index > 0 {
            lines.push(DiffLine {
                op: "…",
                text: String::new(),
            });
        }
        for change in group.iter().flat_map(|op| diff.iter_changes(op)) {
            let op = match change.tag() {
                ChangeTag::Equal => "=",
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
            };
            lines.push(DiffLine {
                op,
                text: change.value().trim_end_matches(['\n', '\r']).to_owned(),
            });
        }
    }
    lines
}

fn diff_part(part: &str, old: Option<&str>, new: Option<&str>) -> DiffPart {
    let status = match (old, new) {
        (None, _) => PartStatus::Added,
        (_, None) => PartStatus::Removed,
        (Some(old), Some(new)) if old == new => PartStatus::Same,
        (Some(_), Some(_)) => PartStatus::Changed,
    };
    let lines = if status == PartStatus::Same {
        Vec::new()
    } else {
        diff_text(old.unwrap_or_default(), new.unwrap_or_default())
    };
    DiffPart {
        part: part.to_owned(),
        status,
        lines,
    }
}

/// What changed from one version of a skill to another, part by part: the description,
/// the instructions, then each file (those of the new version first).
///
/// # Arguments
///
/// * `old` - The version before; `None` for a new skill
/// * `new` - The version after
#[must_use]
pub fn diff(old: Option<&SkillDraft>, new: &SkillDraft) -> Vec<DiffPart> {
    let old_file = |name: &str| {
        old.and_then(|old| old.files.iter().find(|file| file.name == name))
            .map(|file| file.content.as_str())
    };
    let mut parts = vec![
        diff_part(
            "description",
            old.map(|old| old.description.as_str()),
            Some(&new.description),
        ),
        diff_part("instructions", old.map(|old| old.content.as_str()), Some(&new.content)),
    ];
    parts.extend(
        new.files
            .iter()
            .map(|file| diff_part(&file.name, old_file(&file.name), Some(&file.content))),
    );
    parts.extend(
        old.iter()
            .flat_map(|old| old.files.iter())
            .filter(|file| !new.files.iter().any(|kept| kept.name == file.name))
            .map(|file| diff_part(&file.name, Some(&file.content), None)),
    );
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(files: Vec<SkillFile>) -> SkillDraft {
        SkillDraft {
            description: "  Weather for a city.  ".to_owned(),
            content: "Run forecast.py".to_owned(),
            files,
        }
    }

    fn file(name: &str, content: &str) -> SkillFile {
        SkillFile {
            name: name.to_owned(),
            content: content.to_owned(),
        }
    }

    #[test]
    fn diffs_show_each_part_with_context_around_changes() {
        // Arrange
        let numbered = |changed: usize| -> String {
            (1..=20)
                .map(|line| {
                    if line == changed {
                        "changed".to_owned()
                    } else {
                        format!("line {line}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let old = SkillDraft {
            description: "Weather.".to_owned(),
            content: numbered(0),
            files: vec![file("old.py", "a"), file("same.py", "s")],
        };
        let new = SkillDraft {
            description: "Weather.".to_owned(),
            content: numbered(10),
            files: vec![file("same.py", "s"), file("new.py", "b\nc")],
        };

        // Act
        let parts = diff(Some(&old), &new);
        let fresh = diff(None, &new);

        // Assert
        let summary: Vec<(&str, PartStatus)> = parts.iter().map(|part| (part.part.as_str(), part.status)).collect();
        assert_eq!(
            summary,
            [
                ("description", PartStatus::Same),
                ("instructions", PartStatus::Changed),
                ("same.py", PartStatus::Same),
                ("new.py", PartStatus::Added),
                ("old.py", PartStatus::Removed),
            ]
        );
        let ops: String = parts[1].lines.iter().map(|line| line.op).collect();
        assert_eq!(ops, "===-+===", "three lines of context on each side");
        assert_eq!(parts[1].lines[4].text, "changed");
        assert_eq!(parts[3].lines.len(), 2);
        assert!(fresh.iter().all(|part| part.status == PartStatus::Added));
    }

    #[test]
    fn names_are_canonical_and_checked() {
        assert_eq!(clean_name(" Weather-Forecast ").unwrap(), "weather-forecast");
        for bad in ["", "-x", "a b", "a_b", "../x", &"x".repeat(65)] {
            assert!(clean_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn content_and_files_are_checked() {
        let guide = Guide {
            plugin: "finance".to_owned(),
            name: "earnings-call".to_owned(),
            description: "Analysing an earnings call.".to_owned(),
            content: String::new(),
        };
        let (name, cleaned) = prepare("Forecast", draft(vec![file("forecast.py", "print(1)")]), &[]).unwrap();
        assert_eq!(
            (name.as_str(), cleaned.description.as_str()),
            ("forecast", "Weather for a city.")
        );
        assert!(
            prepare("earnings-call", draft(Vec::new()), &[guide])
                .unwrap_err()
                .contains("finance")
        );
        let refused = [
            draft(vec![file("../x.py", "x")]),
            draft(vec![file(".env", "x")]),
            draft(vec![file("a.py", "x"), file("a.py", "y")]),
            draft(vec![file("a.py", "  ")]),
            draft(vec![file("a.py", &"x".repeat(MAX_FILES_CHARS + 1))]),
            draft((0..=MAX_FILES).map(|index| file(&format!("f{index}.py"), "x")).collect()),
            SkillDraft {
                description: " ".to_owned(),
                ..draft(Vec::new())
            },
            SkillDraft {
                content: "x".repeat(MAX_CONTENT_CHARS + 1),
                ..draft(Vec::new())
            },
        ];
        for refused in refused {
            assert!(clean(refused).is_err());
        }
    }
}
