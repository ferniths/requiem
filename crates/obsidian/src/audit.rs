//! Vault auditing: broken wikilinks, orphan notes, and duplicate content.

use crate::vault::Vault;
use crate::wikilink::{extract_wikilinks, is_resolved};
use serde::Serialize;

/// A broken wikilink: a link whose target note does not exist.
#[derive(Debug, Clone, Serialize)]
pub struct BrokenLink {
    /// The note that contains the broken link.
    pub source_note: String,
    /// The unresolved link title.
    pub target: String,
    /// The line number in the source note (1-indexed).
    pub line: usize,
}

/// A note with no inbound wikilinks from any other note.
#[derive(Debug, Clone, Serialize)]
pub struct OrphanNote {
    pub path: String,
}

/// Notes that share the same SHA-256 content hash.
#[derive(Debug, Clone, Serialize)]
pub struct Duplicates {
    pub hash: String,
    pub notes: Vec<String>,
}

/// The result of a full audit pass over the vault.
#[derive(Debug, Clone, Serialize)]
pub struct AuditResult {
    pub broken_links: Vec<BrokenLink>,
    pub orphan_notes: Vec<OrphanNote>,
    pub duplicates: Vec<Duplicates>,
}

/// Run a full audit: broken links, orphans, and duplicates.
pub fn audit(vault: &Vault) -> AuditResult {
    let notes = vault.list_notes();
    let root = vault.root().to_path_buf();

    // --- Broken links ---
    let mut broken_links = Vec::new();
    for note_path in &notes {
        let rel = note_path
            .strip_prefix(&root)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let content = std::fs::read_to_string(note_path).unwrap_or_default();
        for (line_no, line) in content.lines().enumerate() {
            for link in extract_wikilinks(line) {
                if !is_resolved(vault.root(), &link.title) {
                    broken_links.push(BrokenLink {
                        source_note: rel.clone(),
                        target: link.title,
                        line: line_no + 1,
                    });
                }
            }
        }
    }

    // --- Orphans ---
    let mut inbound: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for note_path in &notes {
        let content = std::fs::read_to_string(note_path).unwrap_or_default();
        for link in extract_wikilinks(&content) {
            let target_path = crate::wikilink::resolve_wikilink(vault.root(), &link.title);
            if let Some(tp) = target_path {
                let target_rel = tp
                    .strip_prefix(&root)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                *inbound.entry(target_rel).or_insert(0) += 1;
            }
        }
    }
    let orphan_notes: Vec<OrphanNote> = notes
        .iter()
        .filter_map(|p| {
            let rel = p.strip_prefix(&root).ok()?.to_string_lossy().to_string();
            let count = inbound.get(&rel).copied().unwrap_or(0);
            if count == 0 {
                Some(OrphanNote { path: rel })
            } else {
                None
            }
        })
        .collect();

    // --- Duplicates ---
    let mut by_hash: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for note_path in &notes {
        let content = std::fs::read(note_path).unwrap_or_default();
        let hash = sha256_hex(&content);
        let rel = note_path
            .strip_prefix(&root)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        by_hash.entry(hash).or_default().push(rel);
    }
    let duplicates: Vec<Duplicates> = by_hash
        .into_iter()
        .filter(|(_, notes)| notes.len() > 1)
        .map(|(hash, notes)| Duplicates { hash, notes })
        .collect();

    AuditResult {
        broken_links,
        orphan_notes,
        duplicates,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    hex::encode(&result)
}

// Minimal hex encoder to avoid an extra dependency.
mod hex {
    pub fn encode(data: &[u8]) -> String {
        data.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::Vault;

    fn make_vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("A.md"),
            "# A\n\nLink to [[B]] and [[Missing]]\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("B.md"), "# B\n\nNo links here.\n").unwrap();
        std::fs::write(dir.path().join("Orphan.md"), "# Orphan\n").unwrap();
        std::fs::write(dir.path().join("Dup1.md"), "same content").unwrap();
        std::fs::write(dir.path().join("Dup2.md"), "same content").unwrap();
        let vault = Vault::new(dir.path().to_path_buf());
        (dir, vault)
    }

    #[test]
    fn finds_broken_links() {
        let (_dir, vault) = make_vault();
        let result = audit(&vault);
        let missing: Vec<_> = result
            .broken_links
            .iter()
            .filter(|b| b.target == "Missing")
            .collect();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].source_note, "A.md");
    }

    #[test]
    fn finds_orphans() {
        let (_dir, vault) = make_vault();
        let result = audit(&vault);
        let orphans: Vec<_> = result.orphan_notes.iter().map(|o| o.path.clone()).collect();
        assert!(orphans.contains(&"Orphan.md".to_string()));
    }

    #[test]
    fn finds_duplicates() {
        let (_dir, vault) = make_vault();
        let result = audit(&vault);
        assert!(!result.duplicates.is_empty());
        let dup_notes: Vec<String> = result.duplicates[0].notes.clone();
        assert!(dup_notes.contains(&"Dup1.md".to_string()));
        assert!(dup_notes.contains(&"Dup2.md".to_string()));
    }
}
