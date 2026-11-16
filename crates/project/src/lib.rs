//! REQUIEM project crate.
//!
//! Detects the build ecosystem of a project directory (Cargo, CMake,
//! .NET, Node, Python) so the orchestration layer knows which build
//! commands to run, which test suites to look for, and how to structure
//! agent tasks around the project's toolchain.

use std::path::Path;

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// The detected build ecosystem for a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Ecosystem {
    #[default]
    Unknown,
    Cargo,
    Cmake,
    Dotnet,
    Node,
    Python,
    Go,
    RustCMake,
    Mixed,
}

impl std::fmt::Display for Ecosystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Unknown => "unknown",
            Self::Cargo => "cargo",
            Self::Cmake => "cmake",
            Self::Dotnet => "dotnet",
            Self::Node => "node",
            Self::Python => "python",
            Self::Go => "go",
            Self::RustCMake => "rust+cmake",
            Self::Mixed => "mixed",
        };
        write!(f, "{s}")
    }
}

/// A detected project layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectProfile {
    /// The detected ecosystem.
    pub ecosystem: Ecosystem,
    /// The project root path (as a string for portability).
    pub root: String,
    /// Build command, if detected.
    pub build_command: Option<String>,
    /// Test command, if detected.
    pub test_command: Option<String>,
    /// Lint command, if detected.
    pub lint_command: Option<String>,
    /// The language(s) detected (best-effort).
    pub languages: Vec<String>,
    /// Whether the project has a git repo.
    pub has_git: bool,
}

/// Detect the project ecosystem by walking the directory.
///
/// Returns `None` if the path does not exist.
pub fn detect(root: &Path) -> Option<ProjectProfile> {
    if !root.is_dir() {
        return None;
    }

    let has_cargo = root.join("Cargo.toml").exists();
    let has_cmake = root.join("CMakeLists.txt").exists();
    let has_sln = WalkDir::new(root).max_depth(2).into_iter().any(|e| {
        e.map(|e| e.file_name().to_string_lossy().ends_with(".sln"))
            .unwrap_or(false)
    });
    let has_pkg_json = root.join("package.json").exists();
    let has_pyproject = root.join("pyproject.toml").exists() || root.join("setup.py").exists();
    let has_go_mod = root.join("go.mod").exists();
    let has_git = root.join(".git").is_dir();

    let (ecosystem, build_cmd, test_cmd, lint_cmd) = if has_cargo {
        let has_rust_cmake = has_cmake;
        let eco = if has_rust_cmake {
            Ecosystem::RustCMake
        } else {
            Ecosystem::Cargo
        };
        (
            eco,
            Some("cargo build".to_string()),
            Some("cargo test".to_string()),
            Some("cargo clippy".to_string()),
        )
    } else if has_cmake {
        (
            Ecosystem::Cmake,
            Some("cmake -B build && cmake --build build".to_string()),
            Some("ctest --test-dir build".to_string()),
            None,
        )
    } else if has_sln {
        (
            Ecosystem::Dotnet,
            Some("dotnet build".to_string()),
            Some("dotnet test".to_string()),
            Some("dotnet build -warnaserror".to_string()),
        )
    } else if has_pkg_json {
        (
            Ecosystem::Node,
            Some("npm run build".to_string()),
            Some("npm test".to_string()),
            Some("npm run lint".to_string()),
        )
    } else if has_pyproject {
        (
            Ecosystem::Python,
            Some("python -m build".to_string()),
            Some("pytest".to_string()),
            Some("ruff check".to_string()),
        )
    } else if has_go_mod {
        (
            Ecosystem::Go,
            Some("go build ./...".to_string()),
            Some("go test ./...".to_string()),
            Some("golangci-lint run".to_string()),
        )
    } else {
        (Ecosystem::Unknown, None, None, None)
    };

    // Best-effort language detection from file extensions in the top 3 levels.
    let languages = detect_languages(root);

    Some(ProjectProfile {
        ecosystem,
        root: root.to_string_lossy().to_string(),
        build_command: build_cmd,
        test_command: test_cmd,
        lint_command: lint_cmd,
        languages,
        has_git,
    })
}

/// Detect languages by file extension, sampling the top 3 directory levels.
fn detect_languages(root: &Path) -> Vec<String> {
    let mut found = std::collections::BTreeSet::new();
    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = match ext {
            "rs" => "Rust",
            "c" | "h" => "C",
            "cpp" | "cxx" | "cc" | "hpp" => "C++",
            "cs" => "C#",
            "js" | "ts" | "jsx" | "tsx" => "JavaScript/TypeScript",
            "py" => "Python",
            "go" => "Go",
            "java" => "Java",
            "rb" => "Ruby",
            "swift" => "Swift",
            "kt" | "kts" => "Kotlin",
            "f90" | "f95" => "Fortran",
            "sh" | "bat" | "ps1" => "Shell",
            "md" | "rst" | "txt" => "Docs",
            "toml" | "json" | "yaml" | "yml" => "Config",
            "sql" => "SQL",
            _ => continue,
        };
        found.insert(lang.to_string());
        if found.len() >= 8 {
            break; // cap the scan
        }
    }
    found.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_cargo_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let mut f = std::fs::File::create(dir.path().join("src").join("main.rs")).unwrap();
        let _ = f.write_all(b"fn main() {}").unwrap();

        let profile = detect(dir.path()).unwrap();
        assert_eq!(profile.ecosystem, Ecosystem::Cargo);
        assert_eq!(profile.build_command.as_deref(), Some("cargo build"));
        assert!(profile.languages.contains(&"Rust".to_string()));
    }

    #[test]
    fn detects_node_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        let profile = detect(dir.path()).unwrap();
        assert_eq!(profile.ecosystem, Ecosystem::Node);
    }

    #[test]
    fn detects_python_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        let mut f = std::fs::File::create(dir.path().join("main.py")).unwrap();
        let _ = f.write_all(b"print('hi')").unwrap();

        let profile = detect(dir.path()).unwrap();
        assert_eq!(profile.ecosystem, Ecosystem::Python);
        assert!(profile.languages.contains(&"Python".to_string()));
    }

    #[test]
    fn unknown_ecosystem() {
        let dir = tempfile::tempdir().unwrap();
        let profile = detect(dir.path()).unwrap();
        assert_eq!(profile.ecosystem, Ecosystem::Unknown);
    }

    #[test]
    fn detects_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();

        let profile = detect(dir.path()).unwrap();
        assert!(profile.has_git);
    }
}
