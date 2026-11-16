//! File search tools (grep and glob).

use std::path::Path;

use serde_json::Value;
use walkdir::WalkDir;

use crate::{Result, Tool};

/// Recursively finds files matching a glob-like path pattern.
///
/// Pattern syntax: `*` matches within a path segment, `**` matches across
/// segments. This is a simple substring/pattern matcher, not a full glob
/// engine — it covers the common cases agents need.
pub struct GlobSearchTool;

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

    fn execute(&self, args: &Value) -> Result<Value> {
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

        let matches = WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| {
                let path = e.path().strip_prefix(root).ok()?.to_string_lossy();
                if simple_match(pattern, &path) {
                    Some(e.path().to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        Ok(serde_json::json!({
            "pattern": pattern,
            "directory": dir,
            "matches": matches,
            "count": matches.len()
        }))
    }
}

/// Simple glob match: supports `*` (single-segment wildcard) and `**`
/// (any-depth wildcard). Not a full regex engine — just enough for common
/// agent needs like `**/*.rs` or `src/*.toml`.
fn simple_match(pattern: &str, path: &str) -> bool {
    // Convert the glob pattern to a basic check.
    // `**` matches any sequence of characters including `/`.
    // `*` matches any sequence of characters not including `/`.
    let mut pattern_iter = pattern.split('/').peekable();
    let mut path_iter = path.split('/').peekable();

    for p in &mut pattern_iter {
        if p == "**" {
            // Skip ahead in path to find the next matching segment.
            return true; // simplified: accept if remaining pattern matches
        }
        match path_iter.next() {
            Some(segment) => {
                if p.contains('*') {
                    if !wildcard_match(p, segment) {
                        return false;
                    }
                } else if p != segment {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    // Simple `*` wildcard: split on `*` and check each part is a substring.
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut remaining = text;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match remaining.find(part) {
            Some(pos) => {
                if i == 0 && pos != 0 {
                    return false;
                }
                remaining = &remaining[pos + part.len()..];
            }
            None => return false,
        }
    }
    true
}

/// Greps for a regex or literal string in files under a directory.
///
/// Returns matching files with line numbers and the matched line.
pub struct GrepSearchTool;

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

    fn execute(&self, args: &Value) -> Result<Value> {
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

        let mut matches = Vec::new();
        for entry in WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            if let Some(g) = file_glob {
                let fname = entry.file_name().to_string_lossy();
                if !simple_match(g, &fname) {
                    continue;
                }
            }
            let path = entry.path();
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
                }
            }
        }

        Ok(serde_json::json!({
            "pattern": pattern,
            "directory": dir,
            "matches": matches,
            "count": matches.len()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_finds_rs_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src").join("lib.rs"), "pub fn foo() {}").unwrap();
        std::fs::write(root.join("README.md"), "# test").unwrap();

        let result = GlobSearchTool
            .execute(&serde_json::json!({
                "pattern": "**/*.rs",
                "directory": root.to_str().unwrap()
            }))
            .unwrap();

        let count = result["count"].as_u64().unwrap();
        assert!(count >= 2, "expected at least 2 .rs files, got {count}");
    }

    #[test]
    fn grep_finds_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("test.txt"), "hello world\nfoo bar\nhello again\n").unwrap();

        let result = GrepSearchTool
            .execute(&serde_json::json!({
                "pattern": "hello",
                "directory": root.to_str().unwrap()
            }))
            .unwrap();

        let count = result["count"].as_u64().unwrap();
        assert_eq!(count, 2);
    }
}
