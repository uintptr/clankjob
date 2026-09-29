//! Domain types and traits shared by every clankjob crate.
//!
//! This crate has no I/O. It defines what a case, an event, a wait condition and a
//! human request are, and the [`llm::LlmProvider`] trait that LLM adapters implement.

/// Error returned when a stored or received string is not a known enum variant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} `{value}`")]
pub struct ParseEnumError {
    kind: &'static str,
    value: String,
}

/// Declares a fieldless enum that converts to and from fixed `snake_case` strings.
///
/// The strings are used both by serde (JSON) and by the database columns, so the two can
/// never drift apart. Macros defined in the crate root before the `mod` declarations are
/// visible inside every module of the crate.
macro_rules! string_enum {
    ($(#[$doc:meta])* $name:ident { $($(#[$variant_doc:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub enum $name {
            $(
                $(#[$variant_doc])*
                #[serde(rename = $text)]
                $variant,
            )+
        }

        impl $name {
            /// The stable string form used in JSON and in the database.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = crate::ParseEnumError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($text => Ok(Self::$variant),)+
                    _ => Err(crate::ParseEnumError { kind: stringify!($name), value: value.to_owned() }),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

pub mod case;
pub mod channel;
pub mod event;
pub mod file;
pub mod human;
pub mod ids;
pub mod llm;
pub mod tool;
pub mod wait;
