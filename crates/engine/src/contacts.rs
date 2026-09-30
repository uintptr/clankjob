//! The owner's contacts as the agent sees them: `find_contact` and `save_contact`.
//!
//! The agent can look anyone up and add people, but never edit a contact or trust one:
//! a trusted contact's address is what lets an email skip approval, so an agent that
//! could change it could send anywhere unasked.

use chrono::{DateTime, Utc};
use clankjob_core::contact::{Contact, ContactSource, NewContact};
use clankjob_storage::{self as storage, Connection};
use serde_json::{Value, json};

use crate::Result;
use crate::tools::{FindContactArgs, SaveContactArgs};

/// Most contacts `find_contact` returns.
const MAX_FOUND: usize = 20;
/// Longest name, email, phone or note accepted, in characters.
const MAX_FIELD_CHARS: usize = 500;

/// A trimmed, lowercased email address, or why it is not one.
///
/// # Errors
///
/// Returns a message when the text is not a plausible address.
pub fn normalize_email(email: &str) -> std::result::Result<String, String> {
    let email = email.trim().to_lowercase();
    let valid = email.split('@').count() == 2
        && email.split('@').all(|part| !part.is_empty())
        && email.rsplit('@').next().is_some_and(|domain| domain.contains('.'))
        && !email
            .chars()
            .any(|character| character.is_whitespace() || "<>,;\"".contains(character));
    if valid {
        Ok(email)
    } else {
        Err(format!("{email:?} is not an email address"))
    }
}

/// A contact's fields checked and cleaned: name required, blank fields dropped, the
/// email normalized.
///
/// # Errors
///
/// Returns a message naming the bad field.
pub fn clean(contact: NewContact) -> std::result::Result<NewContact, String> {
    let field = |value: Option<String>, what: &str| -> std::result::Result<Option<String>, String> {
        let value = value.map(|text| text.trim().to_owned()).filter(|text| !text.is_empty());
        if value.as_ref().is_some_and(|text| text.chars().count() > MAX_FIELD_CHARS) {
            return Err(format!("{what} is longer than {MAX_FIELD_CHARS} characters"));
        }
        Ok(value)
    };
    let name = field(Some(contact.name), "name")?.ok_or_else(|| "a contact needs a name".to_owned())?;
    let email = field(contact.email, "email")?
        .map(|email| normalize_email(&email))
        .transpose()?;
    Ok(NewContact {
        name,
        email,
        phone: field(contact.phone, "phone")?,
        note: field(contact.note, "note")?,
        trusted: contact.trusted,
    })
}

/// How well a contact matches the query words; `None` if it does not. Every word must
/// match the name, email or note. Then, best first: the whole name or email; every word
/// a word of the name ("robin" for Robin Tremblay); every word the start of one
/// (Robinson); a word inside the name; only the email or note.
fn score(contact: &Contact, query: &str, words: &[String]) -> Option<u8> {
    let name = contact.name.to_lowercase();
    let email = contact.email.clone().unwrap_or_default();
    let note = contact.note.clone().unwrap_or_default().to_lowercase();
    let matches_all = words
        .iter()
        .all(|word| name.contains(word.as_str()) || email.contains(word.as_str()) || note.contains(word.as_str()));
    if !matches_all {
        return None;
    }
    let name_words: Vec<&str> = name.split_whitespace().collect();
    let every = |test: &dyn Fn(&str, &str) -> bool| {
        !words.is_empty() && words.iter().all(|word| name_words.iter().any(|part| test(part, word)))
    };
    Some(if name == query || email == query {
        4
    } else if every(&|part, word| part == word) {
        3
    } else if every(&|part, word| part.starts_with(word)) {
        2
    } else {
        u8::from(words.iter().any(|word| name.contains(word.as_str())))
    })
}

fn shown(contact: &Contact) -> Value {
    json!({
        "name": contact.name,
        "email": contact.email,
        "phone": contact.phone,
        "note": contact.note,
        "trusted": contact.trusted,
    })
}

/// `find_contact`: the contacts matching a query, best first.
///
/// # Errors
///
/// Returns a storage error.
pub(crate) fn find(connection: &Connection, args: &FindContactArgs) -> Result<Value> {
    let query = args.query.trim().to_lowercase();
    let words: Vec<String> = query.split_whitespace().map(str::to_owned).collect();
    let contacts = storage::contacts::list_contacts(connection)?;
    let mut found: Vec<(u8, &Contact)> = contacts
        .iter()
        .filter_map(|contact| score(contact, &query, &words).map(|rank| (rank, contact)))
        .collect();
    // Stable: equal ranks keep the list's order, by name.
    found.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
    let total = found.len();
    let mut result = json!({
        "query": args.query,
        "contacts": found.iter().take(MAX_FOUND).map(|(_, contact)| shown(contact)).collect::<Vec<_>>(),
    });
    let note = match total {
        0 if contacts.is_empty() => Some("The owner has no contacts yet. Ask them for the address.".to_owned()),
        0 => Some("No contact matches. Ask the owner rather than guessing an address.".to_owned()),
        1 => None,
        more if more > MAX_FOUND => Some(format!(
            "{more} match; the first {MAX_FOUND} are shown. Narrow the query."
        )),
        _ if words.is_empty() => None,
        _ => Some("Several contacts match: if it is not clear which one is meant, ask the owner.".to_owned()),
    };
    if let (Some(note), Some(object)) = (note, result.as_object_mut()) {
        object.insert("note".to_owned(), Value::String(note));
    }
    Ok(result)
}

/// `save_contact`: add a contact, never trusted, unless one with that name or email exists.
///
/// # Returns
///
/// The result for the LLM, or a message when the arguments are invalid
///
/// # Errors
///
/// Returns a storage error.
pub(crate) fn save(
    connection: &Connection,
    args: &SaveContactArgs,
    now: DateTime<Utc>,
) -> Result<std::result::Result<Value, String>> {
    let contact = match clean(NewContact {
        name: args.name.clone(),
        email: args.email.clone(),
        phone: args.phone.clone(),
        note: args.note.clone(),
        trusted: false,
    }) {
        Ok(contact) => contact,
        Err(message) => return Ok(Err(message)),
    };
    let existing = storage::contacts::list_contacts(connection)?.into_iter().find(|known| {
        known.name.eq_ignore_ascii_case(&contact.name)
            || (contact.email.is_some() && known.email.as_deref().map(str::to_lowercase) == contact.email)
    });
    if let Some(known) = existing {
        return Ok(Ok(json!({
            "status": "already_known",
            "contact": shown(&known),
            "note": "Nothing was changed: you can add contacts, not edit them. Tell the owner if their details look wrong.",
        })));
    }
    let stored = storage::contacts::insert_contact(connection, &contact, ContactSource::Agent, now)?;
    Ok(Ok(json!({ "status": "saved", "contact": shown(&stored) })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDb;

    fn add(connection: &Connection, name: &str, email: &str, note: &str, trusted: bool) {
        let contact = NewContact {
            name: name.to_owned(),
            email: Some(email.to_owned()),
            note: Some(note.to_owned()),
            trusted,
            ..NewContact::default()
        };
        storage::contacts::insert_contact(connection, &contact, ContactSource::Owner, Utc::now()).unwrap();
    }

    fn names(found: &Value) -> Vec<String> {
        found["contacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|contact| contact["name"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn contacts_are_found_by_name_email_or_note_best_first() {
        let test_db = TestDb::new();
        let connection = test_db.connect();
        add(&connection, "Robin Tremblay", "robin@sparky.ca", "electrician", true);
        add(&connection, "Bobby Robinson", "bob@plumb.ca", "plumber", false);
        add(&connection, "Me", "noreply@randomail.ca", "the owner", true);
        let find_all = |query: &str| {
            find(
                &connection,
                &FindContactArgs {
                    query: query.to_owned(),
                },
            )
            .unwrap()
        };

        assert_eq!(names(&find_all("robin")), ["Robin Tremblay", "Bobby Robinson"]);
        assert!(find_all("robin")["note"].as_str().unwrap().contains("Several"));
        assert_eq!(names(&find_all("electrician")), ["Robin Tremblay"]);
        assert_eq!(names(&find_all("ROBIN@SPARKY.CA")), ["Robin Tremblay"]);
        assert_eq!(names(&find_all("robin tr")), ["Robin Tremblay"]);
        assert_eq!(find_all("robin tr")["contacts"][0]["trusted"], json!(true));
        assert_eq!(names(&find_all("")).len(), 3);
        assert!(names(&find_all("nobody")).is_empty());
        assert!(find_all("nobody")["note"].as_str().unwrap().contains("Ask the owner"));
    }

    #[test]
    fn the_agent_adds_untrusted_contacts_and_never_edits_one() {
        let test_db = TestDb::new();
        let connection = test_db.connect();
        add(&connection, "Robin Tremblay", "robin@sparky.ca", "electrician", true);
        let save_one = |name: &str, email: Option<&str>| {
            let args = SaveContactArgs {
                name: name.to_owned(),
                email: email.map(str::to_owned),
                phone: None,
                note: Some("  ".to_owned()),
            };
            save(&connection, &args, Utc::now()).unwrap()
        };

        let saved = save_one("Bob Sparks", Some(" Bob@Sparks.CA ")).unwrap();
        let same_name = save_one("robin tremblay", Some("attacker@evil.example")).unwrap();
        let same_email = save_one("Someone Else", Some("ROBIN@sparky.ca")).unwrap();
        let invalid = save_one("Bad", Some("not an address"));

        assert_eq!(
            (saved["status"].as_str(), saved["contact"]["trusted"].as_bool()),
            (Some("saved"), Some(false))
        );
        assert_eq!(saved["contact"]["email"], "bob@sparks.ca");
        assert_eq!(saved["contact"]["note"], Value::Null);
        assert_eq!(same_name["status"], "already_known");
        assert_eq!(same_email["status"], "already_known");
        assert!(invalid.is_err());
        assert_eq!(
            storage::contacts::trusted_emails(&connection).unwrap(),
            ["robin@sparky.ca"],
            "the trusted address never changes"
        );
    }

    #[test]
    fn emails_are_checked() {
        assert_eq!(normalize_email(" Robin@Sparky.CA ").unwrap(), "robin@sparky.ca");
        for bad in ["robin", "robin@", "@sparky.ca", "a@b@c.ca", "robin@localhost", "Robin <r@s.ca>"] {
            assert!(normalize_email(bad).is_err(), "{bad}");
        }
    }
}
