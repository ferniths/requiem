//! REQUIEM obsidian crate.
//!
//! Manages Obsidian vaults: resolves wikilinks, audits broken links,
//! orphans, and duplicates, and writes notes with a change log.

pub mod audit;
pub mod vault;
pub mod wikilink;

pub use audit::{AuditResult, BrokenLink, Duplicates, OrphanNote};
pub use vault::{SearchHit, Vault};
pub use wikilink::{extract_wikilinks, resolve_wikilink};
