//! File search tools (grep and glob).

use std::path::Path;

use serde_json::Value;
use walkdir::WalkDir;

use crate::{Result, Tool};

/// Directories that are never worth searching: VCS internals, build
/// output and dependency caches.
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
];

/// Hard cap on reported matches so a broad pattern cannot flood the
/// model's context window.
const MAX_MATCHES: usize = 500;

/// Maximum file size (bytes) the grep tool will read.
const MAX_GREP_FILE_SIZE: u64 = 1_000_000;

/// Recursively finds files matching a glob-like path pattern.
///
/// Pattern syntax: `*` matches within a path segment, `**` matches across
/// segments, `?` matches a single character.
pub struct GlobSearchTool;

#[async_trait::async_trait]
impl Tool for GlobSearchTool {
    fn name(&self) -> &str {
        "glob_search"
    }

    fn description(&self) -> &str {
        "Recursively find files matching a path pattern under a directory. \
         Arguments: pattern (string), directory (string, optional)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob pattern (e.g. \"**/*.rs\")"},
                "directory": {"type": "string", "description": "Root directory to search from"}
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: &Value) -> Result<Value> {
        let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        let dir = args
            .get("directory")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        if pattern.is_empty() {
            return Err(crate::ToolError::InvalidArgument(
                "empty pattern".to_string(),
            ));
        }

        let root = Path::new(dir);
        if !root.is_dir() {
            return Err(crate::ToolError::Search(format!(
                "directory does not exist: {dir}"
            )));
        }

        let pattern_segments: Vec<&str> = pattern.split('/').collect();
        let mut truncated = false;
        let mut matches: Vec<String> = Vec::new();
        for entry in WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| !under_skipped_dir(e.path(), root))
        {
            if matches.len() >= MAX_MATCHES {
                truncated = true;
                break;
            }
            let rel = match entry.path().strip_prefix(root) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if glob_match(&pattern_segments, &rel) {
                matches.push(entry.path().to_string_lossy().to_string());
            }
        }

        Ok(serde_json::json!({
            "pattern": pattern,
            "directory": dir,
            "matches": matches,
            "count": matches.len(),
            "truncated": truncated
        }))
    }
}

/// Greps for a literal string in files under a directory.
///
/// Returns matching files with line numbers and the matched line.
pub struct GrepSearchTool;

#[async_trait::async_trait]
impl Tool for GrepSearchTool {
    fn name(&self) -> &str {
        "grep_search"
    }

    fn description(&self) -> &str {
        "Search file contents for a literal string under a directory. \
         Arguments: pattern (string), directory (string, optional), \
         glob (optional file filter like \"*.rs\")."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Literal string to search for"},
                "directory": {"type": "string", "description": "Root directory to search"},
                "glob": {"type": "string", "description": "Optional file filter (e.g. \"*.rs\")"}
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: &Value) -> Result<Value> {
        let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        let dir = args
            .get("directory")
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        let file_glob = args.get("glob").and_then(|v| v.as_str());

        if pattern.is_empty() {
            return Err(crate::ToolError::InvalidArgument(
                "empty pattern".to_string(),
            ));
        }

        let root = Path::new(dir);
        if !root.is_dir() {
            return Err(crate::ToolError::Search(format!(
                "directory does not exist: {dir}"
            )));
        }

        // A glob filter with no '/' matches file names; one with a '/'
        // matches the path relative to the search root.
        let glob_segments = file_glob.map(|g| g.split('/').collect::<Vec<&str>>());

        let mut matches = Vec::new();
        let mut truncated = false;
        'files: for entry in WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| !under_skipped_dir(e.path(), root))
        {
            if matches.len() >= MAX_MATCHES {
                truncated = true;
                break;
            }
            let path = entry.path();
            if let Some(segs) = &glob_segments {
                let matched = if segs.len() == 1 {
                    wildcard_match(segs[0], &entry.file_name().to_string_lossy())
                } else {
                    let rel = match path.strip_prefix(root) {
                        Ok(r) => r.to_string_lossy().replace('\\', "/"),
                        Err(_) => continue,
                    };
                    glob_match(segs, &rel)
                };
                if !matched {
                    continue;
                }
            }
            let Ok(meta) = std::fs::metadata(path) else {
                continue;
            };
            if meta.len() > MAX_GREP_FILE_SIZE {
                continue; // skip very large files
            }
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue, // skip binary files
            };
            for (line_no, line) in content.lines().enumerate() {
                if line.contains(pattern) {
                    matches.push(serde_json::json!({
                        "path": path.to_string_lossy(),
                        "line": line_no + 1,
                        "text": line
                    }));
                    if matches.len() >= MAX_MATCHES {
                        truncated = true;
                        break 'files;
                    }
                }
            }
        }

        Ok(serde_json::json!({
            "pattern": pattern,
            "directory": dir,
            "matches": matches,
            "count": matches.len(),
            "truncated": truncated
        }))
    }
}

/// True when `path` (absolute) lives under a directory that should be
/// skipped, relative to `root`.
fn under_skipped_dir(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .map(|rel| {
            rel.components().any(|c| {
                let s = c.as_os_str().to_string_lossy();
                SKIPPED_DIRS.contains(&s.as_ref())
            })
        })
        .unwrap_or(false)
}

/// Full-path glob match: split both pattern and path into `/`-separated
/// segments; `**` consumes any number of segments (including zero),
/// other segments must match one-for-one.
pub fn glob_match(pattern_segments: &[&str], path: &str) -> bool {
    let path_segments: Vec<&str> = path.split('/').collect();
    glob_match_inner(pattern_segments, &path_segments)
}

fn glob_match_inner(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => {
            // `**` may consume 0..=path.len() segments.
            for skip in 0..=path.len() {
                if glob_match_inner(rest, &path[skip..]) {
                    return true;
                }
            }
            false
        }
        Some((p, rest)) => match path.split_first() {
            Some((seg, tail)) if wildcard_match(p, seg) => glob_match_inner(rest, tail),
            _ => false,
        },
    }
}

/// Single-segment wildcard match: `*` matches any run of characters,
/// `?` matches exactly one character. Implemented with dynamic
/// programming so backtracking is linear, not exponential.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (n, m) = (p.len(), t.len());

    // dp[j] = pattern[..i] matches text[..j]
    let mut dp = vec![false; m + 1];
    dp[0] = true;
    for i in 1..=n {
        let pc = p[i - 1];
        let mut next = vec![false; m + 1];
        for j in 0..=m {
            next[j] = match pc {
                '*' => dp[j] || (j > 0 && next[j - 1]),
                '?' => j > 0 && dp[j - 1],
                c => j > 0 && dp[j - 1] && t[j - 1] == c,
            };
        }
        dp = next;
    }
    dp[m]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_double_star_matches_nested() {
        assert!(glob_match(&["**", "*.rs"], "src/main.rs"));
        assert!(glob_match(&["**", "*.rs"], "main.rs"));
        assert!(glob_match(&["**", "*.rs"], "a/b/c/deep.rs"));
    }

    #[test]
    fn glob_double_star_does_not_match_wrong_extension() {
        assert!(!glob_match(&["**", "*.rs"], "README.md"));
        assert!(!glob_match(&["**", "*.rs"], "src/lib.toml"));
    }

    #[test]
    fn glob_single_star_stays_in_segment() {
        assert!(glob_match(&["src", "*.rs"], "src/main.rs"));
        assert!(!glob_match(&["src", "*.rs"], "src/sub/main.rs"));
        assert!(!glob_match(&["src", "*.rs"], "tests/main.rs"));
    }

    #[test]
    fn glob_double_star_mid_pattern() {
        assert!(glob_match(
            &["crates", "**", "src", "*.rs"],
            "crates/tools/src/lib.rs"
        ));
        assert!(glob_match(
            &["crates", "**", "src", "*.rs"],
            "crates/src/main.rs"
        ));
        assert!(!glob_match(
            &["crates", "**", "src", "*.rs"],
            "other/src/main.rs"
        ));
    }

    #[test]
    fn glob_question_mark() {
        assert!(wildcard_match("?.rs", "a.rs"));
        assert!(!wildcard_match("?.rs", "ab.rs"));
    }

    #[test]
    fn glob_star_only_prefix_and_suffix() {
        assert!(wildcard_match("main*", "main.rs"));
        assert!(wildcard_match("*main", "the-main"));
        assert!(!wildcard_match("main*", "notmain.rs"));
    }

    #[test]
    fn glob_exact_segment() {
        assert!(wildcard_match("lib.rs", "lib.rs"));
        assert!(!wildcard_match("lib.rs", "lib.rsx"));
    }

    fn setup_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src").join("deep")).unwrap();
        std::fs::create_dir_all(root.join("target").join("debug")).unwrap();
        std::fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(
            root.join("src").join("deep").join("lib.rs"),
            "pub fn f() {}",
        )
        .unwrap();
        std::fs::write(root.join("README.md"), "# test").unwrap();
        std::fs::write(
            root.join("target").join("debug").join("junk.rs"),
            "// build",
        )
        .unwrap();
        dir
    }

    #[tokio::test]
    async fn glob_finds_rs_files() {
        let dir = setup_tree();
        let root = dir.path();

        let result = GlobSearchTool
            .execute(&serde_json::json!({
                "pattern": "**/*.rs",
                "directory": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let count = result["count"].as_u64().unwrap();
        assert_eq!(count, 2, "expected exactly 2 .rs files (target/ skipped)");
    }

    #[tokio::test]
    async fn glob_skips_target_dir() {
        let dir = setup_tree();
        let root = dir.path();

        let result = GlobSearchTool
            .execute(&serde_json::json!({
                "pattern": "**/junk.rs",
                "directory": root.to_str().unwrap()
            }))
            .await
            .unwrap();
        assert_eq!(result["count"], 0, "target/ must be skipped");
    }

    #[tokio::test]
    async fn grep_finds_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("test.txt"), "hello world\nfoo bar\nhello again\n").unwrap();

        let result = GrepSearchTool
            .execute(&serde_json::json!({
                "pattern": "hello",
                "directory": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let count = result["count"].as_u64().unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn grep_respects_glob_filter() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "needle here\n").unwrap();
        std::fs::write(root.join("b.txt"), "needle here\n").unwrap();

        let result = GrepSearchTool
            .execute(&serde_json::json!({
                "pattern": "needle",
                "directory": root.to_str().unwrap(),
                "glob": "*.rs"
            }))
            .await
            .unwrap();
        assert_eq!(result["count"], 1);
        assert_eq!(
            result["matches"][0]["path"],
            root.join("a.rs").to_string_lossy().as_ref()
        );
    }

    #[tokio::test]
    async fn grep_handles_utf8_paths_and_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("日本語.md"), "ユニコード needle ✓\n").unwrap();

        let result = GrepSearchTool
            .execute(&serde_json::json!({
                "pattern": "needle",
                "directory": root.to_str().unwrap()
            }))
            .await
            .unwrap();
        assert_eq!(result["count"], 1);
        assert_eq!(result["matches"][0]["line"], 1);
    }

    #[tokio::test]
    async fn missing_directory_is_friendly_error() {
        let err = GlobSearchTool
            .execute(&serde_json::json!({"pattern": "**/*", "directory": "Z:/definitely/not/here"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("directory does not exist"));
    }
}
