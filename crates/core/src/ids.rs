//! Strongly typed identifiers.
//!
//! Every id is a ULID string wrapped in its own newtype, so the compiler rejects passing a
//! `CaseId` where a `HumanRequestId` is expected even though both are strings underneath.

use std::fmt;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Declares a ULID-backed id newtype with constructors, accessors and `Display`.
///
/// `macro_rules!` is Rust's pattern-based macro system: each invocation below expands to
/// a full struct definition, which avoids repeating the same boilerplate five times.
macro_rules! ulid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        // `transparent` makes the id serialize as a bare string instead of `{"0": "..."}`.
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generate a new, unique, time-sortable id.
            pub fn generate() -> Self {
                Self(Ulid::generate().to_string())
            }

            /// Wrap an existing id string, e.g. one read from the database or a URL.
            ///
            /// # Arguments
            ///
            /// * `value` - The id as stored or received
            pub fn from_string<S>(value: S) -> Self
            where
                S: Into<String>,
            {
                Self(value.into())
            }

            /// Borrow the id as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

ulid_id!(
    /// Identifies a case.
    CaseId
);
ulid_id!(
    /// Identifies one activation (awake period) of a case.
    ActivationId
);
ulid_id!(
    /// Identifies a wait condition registered by a sleeping case.
    WaitConditionId
);
ulid_id!(
    /// Identifies a human request (a question the case asked its owner).
    HumanRequestId
);
ulid_id!(
    /// Identifies a file uploaded for a case.
    FileId
);
ulid_id!(
    /// Identifies an instruction of a case.
    InstructionId
);
ulid_id!(
    /// Identifies a message queued for a human channel (design §10.3).
    DeliveryId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_unique() {
        // Arrange / Act
        let first = CaseId::generate();
        let second = CaseId::generate();

        // Assert
        assert_ne!(first, second);
    }

    #[test]
    fn id_round_trips_through_string_and_json() {
        // Arrange
        let id = CaseId::from_string("01J9ZZZZZZZZZZZZZZZZZZZZZZ");

        // Act
        let json = serde_json::to_string(&id).unwrap();

        // Assert
        assert_eq!(json, "\"01J9ZZZZZZZZZZZZZZZZZZZZZZ\"");
        assert_eq!(id.as_str(), "01J9ZZZZZZZZZZZZZZZZZZZZZZ");
        assert_eq!(id.to_string(), "01J9ZZZZZZZZZZZZZZZZZZZZZZ");
    }
}
