//! Cross-crate integration tests for the obsidian crate.
//!
//! Exercises the full vault write→read→search→audit round-trip with a
//! sample vault fixture, confirming that atomic writes produce readable
//! notes, search finds them by filename and content, and audit correctly
//! flags broken links, orphans, and duplicates.

use requiem_obsidian::audit::audit;
use requiem_obsidian::Vault;

fn sample_vault() -> (tempfile::TempDir, Vault) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Create subdirectories first (std::fs::write does not create parents).
    std::fs::create_dir_all(root.join("01 Foundations")).unwrap();
    std::fs::create_dir_all(root.join("02 Topics")).unwrap();
    std::fs::create_dir_all(root.join("03 Notes")).unwrap();

    // Build a small vault with inter-linked notes and known issues.
    std::fs::write(
        root.join("01 Foundations/Concept A.md"),
        "# Concept A\n\nSee also [[Concept B]] and [[Ghost Note]]\n",
    )
    .unwrap();

    std::fs::write(
        root.join("01 Foundations/Concept B.md"),
        "# Concept B\n\nLinks back to [[Concept A]]\n",
    )
    .unwrap();

    // An orphan: no other note links to this.
    std::fs::write(root.join("02 Topics/Orphan.md"), "# Orphan\n").unwrap();

    // Two duplicates with identical content.
    std::fs::write(root.join("03 Notes/Dup1.md"), "identical content").unwrap();
    std::fs::write(root.join("03 Notes/Dup2.md"), "identical content").unwrap();

    let vault = Vault::new(root.to_path_buf());
    (dir, vault)
}

#[test]
fn vault_round_trip_write_read_search_audit() {
    let (_dir, vault) = sample_vault();

    // Write a new note via the vault API.
    let entry = vault
        .write_note("04 New/NewNote.md", "# NewNote\n\nSome fresh content\n")
        .unwrap();
    assert_eq!(entry.path, "04 New/NewNote.md");

    // Read it back.
    let content = vault.read_note("04 New/NewNote.md").unwrap();
    assert!(content.contains("# NewNote"));

    // Note count includes all 5 pre-existing + 1 new = 6.
    assert_eq!(vault.note_count(), 6);

    // Search by filename (case-insensitive stem match).
    let hits = vault.search("newnote", 10);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].matched_in_filename);
    assert!(hits[0].path.contains("NewNote.md"));

    // Search by content.
    let hits = vault.search("fresh content", 10);
    assert_eq!(hits.len(), 1);
    assert!(!hits[0].matched_in_filename);
    assert!(hits[0].snippet.contains("fresh content"));

    // Audit: broken links (Ghost Note does not exist).
    let result = audit(&vault);
    let broken_targets: Vec<&str> = result
        .broken_links
        .iter()
        .map(|b| b.target.as_str())
        .collect();
    assert!(
        broken_targets.contains(&"Ghost Note"),
        "Ghost Note must be a broken link, got {broken_targets:?}"
    );

    // Audit: orphans (Orphan.md has no inbound links).
    let orphan_paths: Vec<&str> = result
        .orphan_notes
        .iter()
        .map(|o| o.path.as_str())
        .collect();
    assert!(
        orphan_paths.iter().any(|p| p.contains("Orphan.md")),
        "Orphan.md must be flagged as orphan, got {orphan_paths:?}"
    );

    // Audit: duplicates (Dup1 and Dup2 share content).
    assert!(!result.duplicates.is_empty());
    let dup_notes: Vec<&str> = result.duplicates[0]
        .notes
        .iter()
        .map(|s| s.as_str())
        .collect();
    assert!(dup_notes.iter().any(|p| p.contains("Dup1.md")));
    assert!(dup_notes.iter().any(|p| p.contains("Dup2.md")));

    // Change log was appended.
    let log_path = _dir.path().join(".requiem-changelog.json");
    let raw = std::fs::read_to_string(log_path).unwrap();
    let log: Vec<requiem_obsidian::vault::ChangeLogEntry> = serde_json::from_str(&raw).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].path, "04 New/NewNote.md");
}

#[test]
fn vault_read_rejects_traversal_and_absolute() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new(dir.path().join("vault"));
    std::fs::create_dir_all(vault.root()).unwrap();
    std::fs::write(dir.path().join("secret.txt"), "top secret").unwrap();

    // Parent traversal.
    assert!(vault.read_note("../secret.txt").is_err());
    // Nested traversal.
    assert!(vault.read_note("sub/../../secret.txt").is_err());
    // Absolute path.
    #[cfg(windows)]
    let abs = r"C:\Windows\win.ini";
    #[cfg(unix)]
    let abs = "/etc/passwd";
    assert!(vault.read_note(abs).is_err());
}

#[test]
fn vault_write_creates_nested_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new(dir.path());

    vault.write_note("a/b/c/d/e.md", "deep note").unwrap();
    assert_eq!(vault.read_note("a/b/c/d/e.md").unwrap(), "deep note");
    assert!(dir.path().join("a/b/c/d/e.md").exists());
}
