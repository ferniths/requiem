//! Wikilink extraction and resolution.
//!
//! Obsidian wikilinks look like `[[Note Title]]` or `[[Note Title|display]]`.
//! This module extracts them from markdown text and resolves them to
//! actual `.md` file paths in the vault.

use std::path::Path;

use regex::Regex;

/// A resolved wikilink target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wikilink {
    /// The raw link text as it appeared (without `[[`/`]]`).
    pub raw: String,
    /// The note title part (before `|` if present).
    pub title: String,
    /// The display alias (after `|` if present).
    pub alias: Option<String>,
}

/// Extract all wikilinks from a markdown string.
pub fn extract_wikilinks(text: &str) -> Vec<Wikilink> {
    // Matches [[target]] or [[target|alias]]
    let re = Regex::new(r"\[\[([^\[\]|]+)(?:\|([^\[\]]+))?\]\]").unwrap();
    re.find_iter(text)
        .map(|m| {
            let title = m
                .as_str()
                .trim_start_matches("[[")
                .trim_end_matches("]]")
                .to_string();
            let parts: Vec<&str> = title.split('|').collect();
            let raw_title = parts[0].trim().to_string();
            let alias = parts.get(1).map(|s| s.trim().to_string());
            Wikilink {
                raw: m.as_str().to_string(),
                title: raw_title,
                alias,
            }
        })
        .collect()
}

/// Resolve a wikilink title to a file path relative to the vault root.
///
/// Tries, in order:
/// 1. `<title>.md` at the root
/// 2. a case-insensitive match anywhere in the vault
/// 3. None (unresolved)
pub fn resolve_wikilink(vault_root: &Path, title: &str) -> Option<std::path::PathBuf> {
    // 1. Direct: <root>/<title>.md
    let direct = vault_root.join(format!("{title}.md"));
    if direct.is_file() {
        return Some(direct);
    }

    // 2. Case-insensitive search
    let title_lower = title.to_lowercase();
    for entry in walkdir::WalkDir::new(vault_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.eq_ignore_ascii_case(&title_lower) {
            return Some(path.to_path_buf());
        }
    }

    None
}

/// Check whether a resolved wikilink actually points to an existing file.
pub fn is_resolved(vault_root: &Path, title: &str) -> bool {
    resolve_wikilink(vault_root, title).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_plain_wikilink() {
        let links = extract_wikilinks("See [[My Note]] for details.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].title, "My Note");
        assert_eq!(links[0].alias, None);
    }

    #[test]
    fn extracts_wikilink_with_alias() {
        let links = extract_wikilinks("Go to [[Real Note|display name]] now.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].title, "Real Note");
        assert_eq!(links[0].alias, Some("display name".to_string()));
    }

    #[test]
    fn extracts_multiple_wikilinks() {
        let text = "[[A]] and [[B|b-alias]] and [[C]]";
        let links = extract_wikilinks(text);
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].title, "A");
        assert_eq!(links[1].title, "B");
        assert_eq!(links[2].title, "C");
    }

    #[test]
    fn resolves_existing_note() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Hello World.md"), "content").unwrap();

        let resolved = resolve_wikilink(dir.path(), "Hello World");
        assert!(resolved.is_some());
        assert!(resolved.unwrap().ends_with("Hello World.md"));
    }

    #[test]
    fn unresolved_note_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_wikilink(dir.path(), "Does Not Exist");
        assert!(resolved.is_none());
    }

    #[test]
    fn case_insensitive_resolution() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SomeNote.md"), "x").unwrap();

        let resolved = resolve_wikilink(dir.path(), "somenote");
        assert!(resolved.is_some());
    }
}
