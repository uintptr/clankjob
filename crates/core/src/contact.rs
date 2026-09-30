//! The owner's contacts: people the agent can look up by name.
//!
//! Only the owner marks a contact trusted. Emails to trusted contacts alone need no
//! approval (unless the case says otherwise); the agent can add contacts, never trusted.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::ids::ContactId;

string_enum!(
    /// Who added a contact.
    ContactSource {
        /// The owner, in the web UI or through the API.
        Owner => "owner",
        /// The agent, with `save_contact` during a case.
        Agent => "agent",
    }
);

/// A contact to add, or the new state of one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewContact {
    /// Name, e.g. `Robin Tremblay`.
    pub name: String,
    /// Email address.
    pub email: Option<String>,
    /// Phone number.
    pub phone: Option<String>,
    /// Free text: who they are, how to reach them.
    pub note: Option<String>,
    /// Whether emails to them need no approval. Only the owner sets it.
    pub trusted: bool,
}

/// A stored contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Contact {
    /// Unique id.
    pub id: ContactId,
    /// Name.
    pub name: String,
    /// Email address.
    pub email: Option<String>,
    /// Phone number.
    pub phone: Option<String>,
    /// Free text.
    pub note: Option<String>,
    /// Whether emails to them need no approval.
    pub trusted: bool,
    /// Who added it.
    pub added_by: ContactSource,
    /// When it was added.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}
