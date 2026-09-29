//! Files uploaded for a case: text, PDFs and images the agent can read (design §7.5).

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::ids::{CaseId, FileId};

string_enum!(
    /// What kind of content a file holds, which decides how the agent reads it.
    FileKind {
        /// UTF-8 text of any sort: markdown, plain text, CSV, JSON, source code.
        Text => "text",
        /// A PDF; its text layer is extracted at upload.
        Pdf => "pdf",
        /// A PNG, JPEG, GIF or WebP image, shown to models that can see images.
        Image => "image",
    }
);

/// A stored file. The bytes live on disk; `text` holds what could be extracted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaseFile {
    /// Unique id; also the file's name on disk.
    pub id: FileId,
    /// The case it belongs to.
    pub case_id: CaseId,
    /// Original file name.
    pub name: String,
    /// Media type detected from the content, e.g. `application/pdf`.
    pub media_type: String,
    /// Kind of content.
    pub kind: FileKind,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 of the bytes, hex encoded.
    pub sha256: String,
    /// Extracted text (text files, PDFs with a text layer). Not part of listings.
    #[serde(skip)]
    pub text: Option<String>,
    /// Characters of extracted text, if any.
    pub text_chars: Option<u64>,
    /// Page count, for PDFs.
    pub pages: Option<u32>,
    /// Upload time.
    pub created_at: DateTime<Utc>,
}
