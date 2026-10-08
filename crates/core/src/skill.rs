//! Skills: what a case learned and saved for later cases (design §9.10), instructions
//! read like a plugin guide, optionally with scripts run in the sandbox.
//!
//! Every save is a new version, so the owner can see what changed and go back. Only the
//! owner's approval (or the owner's own edit) makes a version current.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{CaseId, HumanRequestId};

string_enum!(
    /// Who saved a version of a skill.
    SkillAuthor {
        /// The owner, in the web UI or through the API.
        Owner => "owner",
        /// A case, with `save_skill`, approved by the owner.
        Agent => "agent",
    }
);

/// A file of a skill, e.g. a script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillFile {
    /// File name, a single path component, e.g. `forecast.py`.
    pub name: String,
    /// Its text.
    pub content: String,
}

/// What a skill says: the content of one version.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillDraft {
    /// When it applies, shown in every case's system prompt.
    pub description: String,
    /// The instructions, markdown: when and how to use it, how to call its scripts.
    pub content: String,
    /// Scripts and other files, copied to the sandbox when a command uses the skill.
    #[serde(default)]
    pub files: Vec<SkillFile>,
}

/// One saved version of a skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillVersion {
    /// The skill's name.
    pub skill: String,
    /// 1 for the first save, then one more for each.
    pub version: u32,
    /// What it says.
    #[serde(flatten)]
    pub draft: SkillDraft,
    /// Who saved it.
    pub saved_by: SkillAuthor,
    /// The case that saved it, unless that case was deleted since.
    pub case_id: Option<CaseId>,
    /// The approval that let it in, for a version a case saved.
    pub approval_id: Option<HumanRequestId>,
    /// When it was saved.
    pub created_at: DateTime<Utc>,
}

/// A skill as listed: its current version without the content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillSummary {
    /// Unique name, e.g. `weather-forecast`.
    pub name: String,
    /// When it applies.
    pub description: String,
    /// Disabled skills are kept but not shown to cases.
    pub enabled: bool,
    /// Its current version.
    pub version: u32,
    /// Names of its files.
    pub files: Vec<String>,
    /// Who saved the first version.
    pub created_by: SkillAuthor,
    /// The case that saved the first version, if any and not deleted.
    pub created_by_case: Option<CaseId>,
    /// When it was first saved.
    pub created_at: DateTime<Utc>,
    /// When it last changed (a version, or enabled or disabled).
    pub updated_at: DateTime<Utc>,
    /// How many cases read or ran it.
    pub cases: u32,
    /// When a case last read or ran it.
    pub last_used_at: Option<DateTime<Utc>>,
}

/// A case that read or ran a skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillUse {
    /// The case.
    pub case_id: CaseId,
    /// Its title.
    pub title: String,
    /// How many times.
    pub uses: u32,
    /// The first time.
    pub first_used_at: DateTime<Utc>,
    /// The last time.
    pub last_used_at: DateTime<Utc>,
}
