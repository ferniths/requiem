//! Vault management: loads a directory as an Obsidian vault, indexes
//! notes, and provides safe atomic writes with a change log.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// Process-wide counter for generating unique temp filenames.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Build a unique temp path in `dir` for the target file named by
/// `file_name`. The name includes a timestamp, PID, and process-wide
/// counter so concurrent writers cannot collide.
fn unique_temp_path(dir: &Path, file_name: &Path) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() % 1_000_000)
        .unwrap_or(0);
    let pid = std::process::id();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = file_name
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("note");
    let ext = file_name
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("md");
    dir.join(format!(".{stem}.{pid}.{ts}.{n}.tmp.{ext}"))
}

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

    /// List all markdown files in the vault (recursive, includes
    /// subdirectories).
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

    /// Resolve a path relative to the vault root, refusing anything that
    /// could escape it.
    ///
    /// Rejects absolute paths (including Windows drive/UNC prefixes) and any
    /// `..` component. The caller still verifies the canonicalized result
    /// against the canonical root, which catches symlinks that point outside
    /// the vault.
    fn resolve_relative(&self, rel_path: &str) -> Result<PathBuf, VaultError> {
        if rel_path.trim().is_empty() {
            return Err(VaultError::InvalidPath(rel_path.to_string()));
        }
        let rel = Path::new(rel_path);
        if rel.is_absolute() {
            return Err(VaultError::OutsideVault(rel_path.to_string()));
        }
        for comp in rel.components() {
            match comp {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(VaultError::OutsideVault(rel_path.to_string()));
                }
            }
        }
        Ok(self.root.join(rel))
    }

    /// The canonical vault root (resolving symlinks in the configured path).
    fn canonical_root(&self) -> Result<PathBuf, VaultError> {
        std::fs::canonicalize(&self.root).map_err(|e| VaultError::Io(e.to_string()))
    }
    /// Confirm that `candidate` (already canonicalized) is inside `root`
    /// (also canonicalized).
    fn ensure_contained(root: &Path, candidate: &Path, original: &str) -> Result<(), VaultError> {
        if candidate.starts_with(root) {
            Ok(())
        } else {
            Err(VaultError::OutsideVault(original.to_string()))
        }
    }

    /// Read a note by its path relative to the vault root.
    pub fn read_note(&self, rel_path: &str) -> Result<String, VaultError> {
        let path = self.resolve_relative(rel_path)?;
        if !path.is_file() {
            return Err(VaultError::NotFound(rel_path.to_string()));
        }
        // Canonicalize to defeat symlinks that point outside the vault.
        let root = self.canonical_root()?;
        let real = std::fs::canonicalize(&path).map_err(|e| VaultError::Io(e.to_string()))?;
        Self::ensure_contained(&root, &real, rel_path)?;
        std::fs::read_to_string(&real).map_err(|e| VaultError::Io(e.to_string()))
    }

    /// Write a note atomically: write to a uniquely-named temp file in
    /// the same directory, then rename over the target. Appends to the
    /// change log.
    ///
    /// The temp filename includes a process-wide counter and timestamp so
    /// concurrent writers to the same note cannot collide on a shared
    /// temp path.
    pub fn write_note(&self, rel_path: &str, content: &str) -> Result<ChangeLogEntry, VaultError> {
        let path = self.resolve_relative(rel_path)?;
        // Ensure the configured vault root exists first: `canonicalize` needs
        // a real directory, and the root itself is trusted configuration.
        std::fs::create_dir_all(&self.root).map_err(|e| VaultError::Io(e.to_string()))?;
        let root = self.canonical_root()?;
        let file_name = path
            .file_name()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| VaultError::InvalidPath(rel_path.to_string()))?
            .to_owned();

        // Create the parent, then canonicalize it to verify that the real
        // directory is still inside the vault (a symlinked subdirectory
        // could otherwise redirect the write elsewhere).
        let parent = path
            .parent()
            .ok_or_else(|| VaultError::InvalidPath(rel_path.to_string()))?;
        std::fs::create_dir_all(parent).map_err(|e| VaultError::Io(e.to_string()))?;
        let real_parent =
            std::fs::canonicalize(parent).map_err(|e| VaultError::Io(e.to_string()))?;
        Self::ensure_contained(&root, &real_parent, rel_path)?;
        let target = real_parent.join(&file_name);

        // Atomic write: unique temp file in same directory, then rename.
        let tmp = unique_temp_path(
            &real_parent,
            Path::new(file_name.to_str().unwrap_or("note")),
        );
        std::fs::write(&tmp, content).map_err(|e| VaultError::Io(e.to_string()))?;
        if std::fs::rename(&tmp, &target).is_err() {
            // Clean up the temp file on failure so we don't leave debris.
            let _ = std::fs::remove_file(&tmp);
            // Retry once: the target may have been modified by another
            // writer between our read and our rename.
            std::fs::write(&tmp, content).map_err(|e| VaultError::Io(e.to_string()))?;
            std::fs::rename(&tmp, &target).map_err(|e| VaultError::Io(e.to_string()))?;
        }

        // Append to the change log (unique temp + rename for atomicity).
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
        let log_tmp = unique_temp_path(&self.root, std::path::Path::new(".requiem-changelog.json"));
        std::fs::write(&log_tmp, log_json).map_err(|e| VaultError::Io(e.to_string()))?;
        if std::fs::rename(&log_tmp, &log_path).is_err() {
            let _ = std::fs::remove_file(&log_tmp);
            return Err(VaultError::Io(
                "failed to atomically update changelog".to_string(),
            ));
        }

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
    #[error("path escapes the vault root: {0}")]
    OutsideVault(String),
    #[error("invalid note path: {0}")]
    InvalidPath(String),
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

    #[test]
    fn read_rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().join("vault"));
        std::fs::create_dir(dir.path().join("vault")).unwrap();
        std::fs::write(dir.path().join("secret.txt"), "top secret").unwrap();

        let err = vault.read_note("../secret.txt").unwrap_err();
        assert!(
            matches!(err, VaultError::OutsideVault(_)),
            "expected OutsideVault, got {err:?}"
        );
        // Nested traversal in the middle of a path is rejected too.
        let err = vault.read_note("notes/../../secret.txt").unwrap_err();
        assert!(matches!(err, VaultError::OutsideVault(_)), "got {err:?}");
    }

    #[test]
    fn write_rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().join("vault"));
        std::fs::create_dir(dir.path().join("vault")).unwrap();

        let err = vault.write_note("../escaped.md", "nope").unwrap_err();
        assert!(matches!(err, VaultError::OutsideVault(_)), "got {err:?}");
        assert!(
            !dir.path().join("escaped.md").exists(),
            "traversal must not create a file outside the vault"
        );
    }

    #[test]
    fn absolute_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());

        #[cfg(unix)]
        let absolute = "/etc/passwd";
        #[cfg(windows)]
        let absolute = r"C:\Windows\win.ini";

        let err = vault.read_note(absolute).unwrap_err();
        assert!(matches!(err, VaultError::OutsideVault(_)), "got {err:?}");
        let err = vault.write_note(absolute, "x").unwrap_err();
        assert!(matches!(err, VaultError::OutsideVault(_)), "got {err:?}");
    }

    #[test]
    fn empty_path_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());
        assert!(matches!(
            vault.read_note("").unwrap_err(),
            VaultError::InvalidPath(_)
        ));
        assert!(matches!(
            vault.write_note("   ", "x").unwrap_err(),
            VaultError::InvalidPath(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let vault_root = dir.path().join("vault");
        std::fs::create_dir(&vault_root).unwrap();
        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "outside content").unwrap();
        std::os::unix::fs::symlink(&outside, vault_root.join("link.md")).unwrap();

        let vault = Vault::new(vault_root);
        let err = vault.read_note("link.md").unwrap_err();
        assert!(
            matches!(err, VaultError::OutsideVault(_)),
            "symlink escape not caught: {err:?}"
        );
    }

    #[test]
    fn nested_write_then_read_inside_root_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());
        vault.write_note("a/b/c.md", "deep").unwrap();
        assert_eq!(vault.read_note("a/b/c.md").unwrap(), "deep");
        // A `.` component is harmless and stays inside the vault.
        assert_eq!(vault.read_note("./a/b/c.md").unwrap(), "deep");
    }
}
