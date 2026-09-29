//! Files added to a case (design §7.5): detection, text extraction, storage on disk, and
//! how each file is presented to the agent.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use clankjob_core::file::{CaseFile, FileKind};
use clankjob_core::ids::FileId;
use clankjob_core::llm::ImageData;
use serde::Serialize;

/// Largest file accepted, in bytes.
pub const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
/// Most files one case can hold.
pub const MAX_CASE_FILES: usize = 20;
/// Largest total size of a case's files, in bytes.
pub const MAX_CASE_FILE_BYTES: u64 = 100 * 1024 * 1024;
/// Longest file name, in characters.
const MAX_NAME_CHARS: usize = 200;

/// What the content of an uploaded file turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspected {
    /// Media type detected from the content.
    pub media_type: String,
    /// Kind of content.
    pub kind: FileKind,
    /// Extracted text, when there is any.
    pub text: Option<String>,
    /// Page count, for PDFs.
    pub pages: Option<u32>,
}

/// Keep only the last path component and check the name is usable.
///
/// # Errors
///
/// Returns a message if the name is empty or too long.
pub fn clean_name(name: &str) -> Result<String, String> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("file names must be 1 to {MAX_NAME_CHARS} characters"));
    }
    Ok(name.to_owned())
}

fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

fn text_type(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("md" | "markdown") => "text/markdown",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        _ => "text/plain",
    }
}

/// Text and page count of a PDF. A PDF without a text layer (usually a scan) or one the
/// extractor cannot handle yields no text rather than an error.
fn inspect_pdf(bytes: &[u8]) -> (Option<String>, Option<u32>) {
    let pages = lopdf::Document::load_mem(bytes)
        .ok()
        .and_then(|document| u32::try_from(document.get_pages().len()).ok());
    // The extractor panics on some malformed PDFs; a bad upload must not take a
    // request thread down with it.
    let text = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes))
        .ok()
        .and_then(Result::ok)
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty());
    (text, pages)
}

/// Work out what a file is from its bytes (never from what the client claims).
///
/// # Errors
///
/// Returns a message for files that are neither text, PDF nor a supported image.
pub fn inspect(name: &str, bytes: &[u8]) -> Result<Inspected, String> {
    if bytes.starts_with(b"%PDF-") {
        let (text, pages) = inspect_pdf(bytes);
        return Ok(Inspected {
            media_type: "application/pdf".to_owned(),
            kind: FileKind::Pdf,
            text,
            pages,
        });
    }
    if let Some(media_type) = image_type(bytes) {
        return Ok(Inspected {
            media_type: media_type.to_owned(),
            kind: FileKind::Image,
            text: None,
            pages: None,
        });
    }
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => Ok(Inspected {
            media_type: text_type(name).to_owned(),
            kind: FileKind::Text,
            text: Some(text.to_owned()),
            pages: None,
        }),
        _ => Err(format!(
            "`{name}` is not a supported file: use text, PDF, or a PNG, JPEG, GIF or WebP image"
        )),
    }
}

/// Where file bytes live: one file per id in a directory on the data volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// A store in `dir`, created on first write.
    pub fn new<P>(dir: P) -> Self
    where
        P: AsRef<Path>,
    {
        Self {
            dir: dir.as_ref().to_path_buf(),
        }
    }

    fn path(&self, id: &FileId) -> PathBuf {
        self.dir.join(id.as_str())
    }

    /// Write a file's bytes. Written beside and renamed, so a crash never leaves half a file.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the directory or file cannot be written.
    pub fn write(&self, id: &FileId, bytes: &[u8]) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let partial = self.dir.join(format!("{}.partial", id.as_str()));
        fs::write(&partial, bytes)?;
        fs::rename(partial, self.path(id))
    }

    /// Read a file's bytes.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file cannot be read.
    pub fn read(&self, id: &FileId) -> io::Result<Vec<u8>> {
        fs::read(self.path(id))
    }

    /// Remove a file's bytes; used to undo a write whose database insert failed.
    pub fn remove(&self, id: &FileId) {
        let _ = fs::remove_file(self.path(id));
    }

    /// An image file encoded for the LLM, if it can be read.
    #[must_use]
    pub fn image(&self, file: &CaseFile) -> Option<ImageData> {
        let bytes = self.read(&file.id).ok()?;
        Some(ImageData {
            media_type: file.media_type.clone(),
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }
}

/// How the agent gets at a file's content. Files are never pasted into the prompt:
/// they are read on demand, so they cost context only when actually needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Its text is read in parts with `read_file`.
    ReadFile,
    /// It is looked at with `view_image`.
    ViewImage,
    /// Its content cannot be read (a scanned PDF, or an image for a model without vision).
    None,
}

/// A file as described to the agent in the prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileView {
    /// File name.
    pub name: String,
    /// Kind of content.
    pub kind: FileKind,
    /// Human-readable size.
    pub size: String,
    /// Page count, for PDFs.
    pub pages: Option<u32>,
    /// How the agent gets at the content.
    pub access: Access,
    /// Why the content cannot be read, when it cannot.
    pub note: Option<String>,
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{} KB", bytes.div_ceil(1024)),
        _ => {
            let tenths = bytes.saturating_mul(10) / 1_048_576;
            format!("{}.{} MB", tenths / 10, tenths % 10)
        }
    }
}

/// Describe each file for the prompt: which tool reads it, or why it cannot be read.
///
/// # Arguments
///
/// * `files` - The case's files, in upload order
/// * `vision` - Whether the case's model can see images
#[must_use]
pub fn views(files: &[CaseFile], vision: bool) -> Vec<FileView> {
    files
        .iter()
        .map(|file| {
            let (access, note) = match (file.kind, &file.text) {
                (FileKind::Image, _) if vision => (Access::ViewImage, None),
                (FileKind::Image, _) => (Access::None, Some("An image, but the current model cannot see images.")),
                (_, None) => (
                    Access::None,
                    Some("A PDF with no text layer (probably scanned); its text cannot be read."),
                ),
                (_, Some(_)) => (Access::ReadFile, None),
            };
            FileView {
                name: file.name.clone(),
                kind: file.kind,
                size: human_size(file.size),
                pages: file.pages,
                access,
                note: note.map(str::to_owned),
            }
        })
        .collect()
}

/// Find the file the agent means: by id, by exact name, then by name ignoring case.
///
/// # Errors
///
/// Returns a message for the agent if no file, or more than one, matches.
pub fn find<'a>(files: &'a [CaseFile], wanted: &str) -> Result<&'a CaseFile, String> {
    if let Some(file) = files.iter().find(|file| file.id.as_str() == wanted) {
        return Ok(file);
    }
    let exact: Vec<&CaseFile> = files.iter().filter(|file| file.name == wanted).collect();
    let matches = if exact.is_empty() {
        files.iter().filter(|file| file.name.eq_ignore_ascii_case(wanted)).collect()
    } else {
        exact
    };
    match matches.as_slice() {
        [file] => Ok(file),
        [] => {
            let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
            Err(format!("no file named `{wanted}`; files: {}", names.join(", ")))
        }
        several => {
            let ids: Vec<&str> = several.iter().map(|file| file.id.as_str()).collect();
            Err(format!(
                "several files are named `{wanted}`; pass one of these ids instead: {}",
                ids.join(", ")
            ))
        }
    }
}

/// A slice of a text of `max_chars` characters starting at character `offset`.
///
/// # Returns
///
/// The slice and the offset to continue from, if the text goes on
#[must_use]
pub fn chunk(text: &str, offset: usize, max_chars: usize) -> (&str, Option<usize>) {
    let start = text.char_indices().nth(offset).map_or(text.len(), |(index, _)| index);
    let rest = text.get(start..).unwrap_or_default();
    match rest.char_indices().nth(max_chars) {
        Some((end, _)) => (
            rest.get(..end).unwrap_or_default(),
            Some(offset.saturating_add(max_chars)),
        ),
        None => (rest, None),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use clankjob_core::ids::CaseId;

    use super::*;

    /// A one-page PDF whose text layer says `text`, built with lopdf.
    fn pdf(text: &str) -> Vec<u8> {
        use lopdf::content::{Content, Operation};
        use lopdf::{Document, Object, Stream, dictionary};
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id =
            document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier" });
        let resources_id = document.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let leaf_id =
            document.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => content_id });
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![leaf_id.into()], "Count" => 1, "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let catalog_id = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    fn case_file(name: &str, kind: FileKind, text: Option<&str>) -> CaseFile {
        CaseFile {
            id: FileId::generate(),
            case_id: CaseId::from_string("case"),
            name: name.to_owned(),
            media_type: "text/plain".to_owned(),
            kind,
            size: 2048,
            sha256: String::new(),
            text: text.map(str::to_owned),
            text_chars: text.map(|text| text.chars().count() as u64),
            pages: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn pdfs_are_detected_and_their_text_extracted() {
        let inspected = inspect("quote.pdf", &pdf("Total: 1450 dollars")).unwrap();

        assert_eq!(inspected.kind, FileKind::Pdf);
        assert_eq!(inspected.media_type, "application/pdf");
        assert_eq!(inspected.text.as_deref(), Some("Total: 1450 dollars"));
        assert_eq!(inspected.pages, Some(1));
    }

    #[test]
    fn a_broken_pdf_is_kept_without_text() {
        let inspected = inspect("scan.pdf", b"%PDF-1.4 garbage").unwrap();

        assert_eq!(inspected.kind, FileKind::Pdf);
        assert_eq!(inspected.text, None);
    }

    #[test]
    fn images_are_detected_from_their_bytes_not_their_name() {
        let png = b"\x89PNG\r\n\x1a\nrest";
        let webp = b"RIFF\x00\x00\x00\x00WEBPVP8 ";

        assert_eq!(inspect("photo.txt", png).unwrap().media_type, "image/png");
        assert_eq!(
            inspect("a.jpg", &[0xFF, 0xD8, 0xFF, 0xE0]).unwrap().media_type,
            "image/jpeg"
        );
        assert_eq!(inspect("a", b"GIF89a....").unwrap().media_type, "image/gif");
        assert_eq!(inspect("a", webp).unwrap().kind, FileKind::Image);
    }

    #[test]
    fn utf8_is_text_and_other_binaries_are_rejected() {
        let markdown = inspect("tone.md", "# Tone\nBe polite, é".as_bytes()).unwrap();
        let csv = inspect("prices.CSV", b"a,b\n1,2").unwrap();

        assert_eq!(
            (markdown.kind, markdown.media_type.as_str()),
            (FileKind::Text, "text/markdown")
        );
        assert_eq!(csv.media_type, "text/csv");
        assert!(inspect("app.exe", b"MZ\x90\x00\x03\x00\xff\xfe").is_err());
        assert!(inspect("nul.txt", b"a\0b").is_err());
    }

    #[test]
    fn names_are_reduced_to_their_last_component() {
        assert_eq!(clean_name("C:\\Users\\joe\\quote.pdf").unwrap(), "quote.pdf");
        assert_eq!(clean_name("../../etc/passwd").unwrap(), "passwd");
        assert!(clean_name("folder/").is_err());
        assert!(clean_name(&"x".repeat(201)).is_err());
    }

    #[test]
    fn files_are_read_on_demand_never_inlined() {
        // Arrange
        let files = [
            case_file("tone.md", FileKind::Text, Some("Be polite.")),
            case_file("scan.pdf", FileKind::Pdf, None),
            case_file("panel.jpg", FileKind::Image, None),
        ];

        // Act
        let with_vision = views(&files, true);
        let without = views(&files, false);

        // Assert
        let access: Vec<Access> = with_vision.iter().map(|view| view.access).collect();
        assert_eq!(access, [Access::ReadFile, Access::None, Access::ViewImage]);
        assert_eq!(with_vision[0].size, "2 KB");
        assert_eq!(without[2].access, Access::None);
        assert!(without[2].note.as_deref().unwrap().contains("cannot see images"));
    }

    #[test]
    fn files_are_found_by_id_name_or_case_insensitive_name() {
        let files = [
            case_file("Quote.pdf", FileKind::Pdf, Some("a")),
            case_file("a.md", FileKind::Text, Some("b")),
            case_file("a.md", FileKind::Text, Some("c")),
        ];

        assert_eq!(find(&files, "quote.pdf").unwrap().name, "Quote.pdf");
        assert_eq!(find(&files, files[1].id.as_str()).unwrap().text.as_deref(), Some("b"));
        assert!(find(&files, "a.md").unwrap_err().contains("several files"));
        assert!(find(&files, "missing").unwrap_err().contains("files: Quote.pdf, a.md, a.md"));
    }

    #[test]
    fn chunks_split_on_characters_not_bytes() {
        let text = "héllo wörld";

        assert_eq!(chunk(text, 0, 5), ("héllo", Some(5)));
        assert_eq!(chunk(text, 6, 5), ("wörld", None));
        assert_eq!(chunk(text, 50, 5), ("", None));
    }

    #[test]
    fn stored_bytes_round_trip_and_images_are_base64() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("files"));
        let file = CaseFile {
            media_type: "image/png".to_owned(),
            ..case_file("p.png", FileKind::Image, None)
        };

        store.write(&file.id, b"\x89PNG").unwrap();

        assert_eq!(store.read(&file.id).unwrap(), b"\x89PNG");
        assert_eq!(
            store.image(&file).unwrap(),
            ImageData {
                media_type: "image/png".to_owned(),
                base64: "iVBORw==".to_owned()
            }
        );
        store.remove(&file.id);
        assert!(store.read(&file.id).is_err());
    }
}
