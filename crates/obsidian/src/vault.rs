//! Vault management: loads a directory as an Obsidian vault, indexes
//! notes, and provides safe atomic writes with a change log.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// A vault rooted at a filesystem directory.
pub struct Vault {
    root: PathBuf,
}

impl Vault {
    /// Open a vault rooted at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { root: path.into() }
    }

    /// The vault's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the vault directory exists.
    pub fn exists(&self) -> bool {
        self.root.is_dir()
    }

    /// List all markdown files in the vault (non-recursive, top-level only).
    pub fn list_notes(&self) -> Vec<PathBuf> {
        WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "md")
            })
            .map(|e| e.path().to_path_buf())
            .collect()
    }

    /// Read a note by its path relative to the vault root.
    pub fn read_note(&self, rel_path: &str) -> Result<String, VaultError> {
        let path = self.root.join(rel_path);
        if !path.is_file() {
            return Err(VaultError::NotFound(rel_path.to_string()));
        }
        std::fs::read_to_string(&path).map_err(|e| VaultError::Io(e.to_string()))
    }

    /// Write a note atomically: write to a temp file in the same directory,
    /// then rename over the target. Appends to the change log.
    pub fn write_note(&self, rel_path: &str, content: &str) -> Result<ChangeLogEntry, VaultError> {
        let target = self.root.join(rel_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| VaultError::Io(e.to_string()))?;
        }
        // Atomic write: temp file in same directory, then rename.
        let tmp = target.with_file_name(format!(
            ".{}.tmp",
            target
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("note")
        ));
        std::fs::write(&tmp, content).map_err(|e| VaultError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &target).map_err(|e| VaultError::Io(e.to_string()))?;

        // Append to the change log.
        let entry = ChangeLogEntry {
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            path: rel_path.to_string(),
            action: "write".to_string(),
        };
        let log_path = self.root.join(".requiem-changelog.json");
        let mut log: Vec<ChangeLogEntry> = if log_path.is_file() {
            let raw = std::fs::read_to_string(&log_path).unwrap_or_default();
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            Vec::new()
        };
        log.push(entry.clone());
        let log_json = serde_json::to_string_pretty(&log).unwrap_or_default();
        std::fs::write(&log_path, log_json).map_err(|e| VaultError::Io(e.to_string()))?;

        Ok(entry)
    }

    /// The number of markdown notes in the vault.
    pub fn note_count(&self) -> usize {
        self.list_notes().len()
    }

    /// Search notes by case-insensitive substring match on filename and
    /// content. Returns matching notes with the matched line and a short
    /// snippet. Results are capped at `max_results`.
    pub fn search(&self, query: &str, max_results: usize) -> Vec<SearchHit> {
        let q = query.to_lowercase();
        let notes = self.list_notes();
        let root = self.root().to_path_buf();
        let mut hits: Vec<SearchHit> = Vec::new();

        for note_path in &notes {
            if hits.len() >= max_results {
                break;
            }
            let rel = note_path
                .strip_prefix(&root)
                .ok()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            // Filename match
            let file_stem = note_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            let filename_match = file_stem.contains(&q);

            // Content match — first matching line only
            let content = std::fs::read_to_string(note_path).unwrap_or_default();
            let mut content_match: Option<String> = None;
            if !filename_match {
                for line in content.lines() {
                    if line.to_lowercase().contains(&q) {
                        let snippet: String = line.chars().take(120).collect();
                        content_match = Some(snippet);
                        break;
                    }
                }
            }

            if filename_match || content_match.is_some() {
                hits.push(SearchHit {
                    path: rel,
                    matched_in_filename: filename_match,
                    snippet: content_match.unwrap_or_default(),
                });
            }
        }

        hits
    }
}

/// A single search hit returned by [`Vault::search`].
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    /// Note path relative to vault root.
    pub path: String,
    /// Whether the query matched the filename (stem) of the note.
    pub matched_in_filename: bool,
    /// A short snippet from the first matching content line (empty when
    /// only the filename matched).
    pub snippet: String,
}

/// An error from a vault operation.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("note not found: {0}")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(String),
}

/// A single entry in the vault's change log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeLogEntry {
    pub timestamp_ms: u64,
    pub path: String,
    pub action: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());
        vault
            .write_note("01 Foundations/test.md", "# Hello\n\nSome content.")
            .unwrap();

        let content = vault.read_note("01 Foundations/test.md").unwrap();
        assert!(content.contains("# Hello"));
    }

    #[test]
    fn read_nonexistent_errors() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());
        let result = vault.read_note("does-not-exist.md");
        assert!(result.is_err());
    }

    #[test]
    fn note_count_tracks_files() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());
        assert_eq!(vault.note_count(), 0);

        vault.write_note("a.md", "content").unwrap();
        vault.write_note("b.md", "content").unwrap();
        assert_eq!(vault.note_count(), 2);
    }

    #[test]
    fn change_log_is_appended() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());
        vault.write_note("a.md", "v1").unwrap();
        vault.write_note("a.md", "v2").unwrap();

        let log_path = dir.path().join(".requiem-changelog.json");
        let log: Vec<ChangeLogEntry> =
            serde_json::from_str(&std::fs::read_to_string(log_path).unwrap()).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].path, "a.md");
        assert_eq!(log[1].path, "a.md");
    }

    #[test]
    fn search_matches_content_and_filename() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());
        vault.write_note("Alpha.md", "the quick brown fox").unwrap();
        vault
            .write_note("Beta.md", "jumps over the lazy dog")
            .unwrap();

        // Content match
        let hits = vault.search("quick", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "Alpha.md");
        assert!(!hits[0].matched_in_filename);
        assert!(hits[0].snippet.contains("quick"));

        // Filename match
        let hits = vault.search("beta", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "Beta.md");
        assert!(hits[0].matched_in_filename);

        // No match
        let hits = vault.search("nonexistent", 10);
        assert!(hits.is_empty());
    }
}
