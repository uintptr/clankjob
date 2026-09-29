//! The owner's own prompt: `user_prompt.md` in the data directory, a plain text file
//! added to every case's system prompt (design §7.6). It is read on every turn, so an
//! edit, from the web UI or by hand, applies from each case's next turn.

use std::path::Path;

/// Longest user prompt, in characters. It is resent with every LLM turn of every case.
pub const MAX_USER_PROMPT_CHARS: usize = 20_000;

/// The user prompt, or `None` when the file is missing or blank.
///
/// A file that cannot be read is logged and treated as missing, so a bad file never
/// stops cases.
#[must_use]
pub fn read(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "cannot read the user prompt; ignoring it");
            None
        }
    }
}

/// Check a new user prompt against the limit.
///
/// # Errors
///
/// Returns why it cannot be saved.
pub fn validate(content: &str) -> Result<(), String> {
    let chars = content.chars().count();
    if chars > MAX_USER_PROMPT_CHARS {
        return Err(format!(
            "the user prompt is {chars} characters; at most {MAX_USER_PROMPT_CHARS}, since it is resent with every turn"
        ));
    }
    if content.contains('\0') {
        return Err("the user prompt contains a NUL byte".to_owned());
    }
    Ok(())
}

/// Replace the user prompt. The file is written next to its final name and renamed into
/// place, so a reader never sees half of it.
///
/// # Errors
///
/// Returns an I/O error if the file cannot be written.
pub fn write(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("md.partial");
    let mut text = content.trim().to_owned();
    if !text.is_empty() {
        text.push('\n');
    }
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_blank_file_means_no_prompt_and_writes_are_whole() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_prompt.md");

        // Act
        let missing = read(&path);
        write(&path, "  Sign emails as Brad.  \n\n").unwrap();
        let written = read(&path);
        write(&path, "   ").unwrap();
        let blank = read(&path);

        // Assert
        assert_eq!(
            (missing, written.as_deref(), blank),
            (None, Some("Sign emails as Brad."), None)
        );
        assert!(!path.with_extension("md.partial").exists());
        assert!(validate(&"x".repeat(MAX_USER_PROMPT_CHARS + 1)).is_err());
        assert!(validate("Keep emails short.").is_ok());
    }
}
