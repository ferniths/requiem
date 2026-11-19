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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
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
    /// The primary detected ecosystem (first match for backward
    /// compatibility; use `ecosystems` for the full set).
    pub ecosystem: Ecosystem,
    /// All detected ecosystems simultaneously. A monorepo with Rust,
    /// Python, and C++ components will list all three.
    #[serde(default)]
    pub ecosystems: Vec<Ecosystem>,
    /// The project root path (as a string for portability).
    pub root: String,
    /// Build command, if detected (primary ecosystem).
    pub build_command: Option<String>,
    /// Test command, if detected (primary ecosystem).
    pub test_command: Option<String>,
    /// Lint command, if detected (primary ecosystem).
    pub lint_command: Option<String>,
    /// Per-ecosystem build commands (full set).
    #[serde(default)]
    pub build_commands: std::collections::HashMap<String, String>,
    /// Per-ecosystem test commands (full set).
    #[serde(default)]
    pub test_commands: std::collections::HashMap<String, String>,
    /// The language(s) detected (best-effort).
    pub languages: Vec<String>,
    /// Whether the project has a git repo.
    pub has_git: bool,
    /// Subproject roots detected within the main project (monorepo).
    #[serde(default)]
    pub subprojects: Vec<String>,
}

/// Detect the project ecosystem(s) by walking the directory.
///
/// Returns `None` if the path does not exist. Multiple ecosystems can
/// be detected simultaneously (e.g. a Rust + Python + C++ monorepo);
/// the primary `ecosystem` field is the first match for backward
/// compatibility, while `ecosystems` carries the full set.
pub fn detect(root: &Path) -> Option<ProjectProfile> {
    if !root.is_dir() {
        return None;
    }

    let has_cargo = root.join("Cargo.toml").exists();
    let has_cmake = root.join("CMakeLists.txt").exists();
    let has_makefile = root.join("Makefile").exists() || root.join("makefile").exists();
    let has_meson = root.join("meson.build").exists();
    let has_sln = WalkDir::new(root).max_depth(2).into_iter().any(|e| {
        e.map(|e| {
            let name = e.file_name().to_string_lossy();
            name.ends_with(".sln") || name.ends_with(".csproj")
        })
        .unwrap_or(false)
    });
    let has_pkg_json = root.join("package.json").exists();
    let has_pnpm = root.join("pnpm-workspace.yaml").exists();
    let has_yarn = root.join("yarn.lock").exists();
    let has_bun = root.join("bun.lockb").exists();
    let has_ts_config = root.join("tsconfig.json").exists();
    let has_pyproject = root.join("pyproject.toml").exists();
    let has_setup_py = root.join("setup.py").exists();
    let has_requirements = root.join("requirements.txt").exists();
    let has_uv = root.join("uv.lock").exists();
    let has_go_mod = root.join("go.mod").exists();
    let has_git = root.join(".git").is_dir();

    // Collect all detected ecosystems simultaneously.
    let mut ecosystems: Vec<Ecosystem> = Vec::new();
    let mut build_commands: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut test_commands: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    if has_cargo {
        ecosystems.push(Ecosystem::Cargo);
        build_commands.insert("cargo".into(), "cargo build".into());
        test_commands.insert("cargo".into(), "cargo test".into());
    }
    if has_cmake {
        ecosystems.push(Ecosystem::Cmake);
        build_commands.insert(
            "cmake".into(),
            "cmake -B build && cmake --build build".into(),
        );
        test_commands.insert("cmake".into(), "ctest --test-dir build".into());
    }
    if has_makefile {
        ecosystems.push(Ecosystem::Cmake); // Make is a C/C++ build system
        build_commands
            .entry("make".into())
            .or_insert_with(|| "make".into());
        test_commands
            .entry("make".into())
            .or_insert_with(|| "make test".into());
    }
    if has_meson {
        ecosystems.push(Ecosystem::Cmake);
        build_commands
            .entry("meson".into())
            .or_insert_with(|| "meson compile -C builddir".into());
        test_commands
            .entry("meson".into())
            .or_insert_with(|| "meson test -C builddir".into());
    }
    if has_sln {
        ecosystems.push(Ecosystem::Dotnet);
        build_commands.insert("dotnet".into(), "dotnet build".into());
        test_commands.insert("dotnet".into(), "dotnet test".into());
    }
    if has_pkg_json {
        ecosystems.push(Ecosystem::Node);
        let pm = if has_pnpm {
            "pnpm"
        } else if has_yarn {
            "yarn"
        } else if has_bun {
            "bun run"
        } else {
            "npm"
        };
        build_commands.insert("node".into(), format!("{pm} run build"));
        test_commands.insert("node".into(), format!("{pm} test"));
    }
    if has_ts_config {
        // TypeScript is a sub-ecosystem of Node; don't add a separate entry.
        // The `node` build command above already handles it.
    }
    if has_pyproject || has_setup_py || has_requirements || has_uv {
        ecosystems.push(Ecosystem::Python);
        build_commands
            .entry("python".into())
            .or_insert_with(|| "python -m build".into());
        test_commands
            .entry("python".into())
            .or_insert_with(|| "pytest".into());
    }
    if has_go_mod {
        ecosystems.push(Ecosystem::Go);
        build_commands.insert("go".into(), "go build ./...".into());
        test_commands.insert("go".into(), "go test ./...".into());
    }

    // Deduplicate: a C/C++ project with both CMake and Makefile should
    // only appear once.
    let seen: std::collections::BTreeSet<Ecosystem> = ecosystems.iter().copied().collect();
    let ecosystems: Vec<Ecosystem> = seen.into_iter().collect();

    // Discover subprojects: walk 3 levels deep for nested Cargo.toml,
    // CMakeLists.txt, pyproject.toml, package.json, go.mod.
    let mut subprojects: Vec<String> = Vec::new();
    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let name = entry.file_name().to_string_lossy();
        if name == "Cargo.toml"
            || name == "CMakeLists.txt"
            || name == "pyproject.toml"
            || name == "package.json"
            || name == "go.mod"
            || name.ends_with(".sln")
        {
            if let Some(parent) = entry.path().parent() {
                if let Ok(rel) = parent.strip_prefix(root) {
                    let rel_str = rel.to_string_lossy().to_string();
                    // Skip the root itself (empty relative path).
                    if !rel_str.is_empty() {
                        subprojects.push(rel_str);
                    }
                }
            }
        }
    }

    let primary = *ecosystems.first().unwrap_or(&Ecosystem::Unknown);
    let primary_key = primary.to_string();
    let primary_build = build_commands.get(&primary_key).cloned();
    let primary_test = test_commands.get(&primary_key).cloned();
    let lint_cmd = match primary {
        Ecosystem::Cargo => Some("cargo clippy".to_string()),
        Ecosystem::Dotnet => Some("dotnet build -warnaserror".to_string()),
        Ecosystem::Python => Some("ruff check".to_string()),
        Ecosystem::Go => Some("golangci-lint run".to_string()),
        _ => None,
    };

    // Best-effort language detection from file extensions in the top 3 levels.
    let languages = detect_languages(root);

    Some(ProjectProfile {
        ecosystem: primary,
        ecosystems,
        root: root.to_string_lossy().to_string(),
        build_command: primary_build,
        test_command: primary_test,
        lint_command: lint_cmd,
        build_commands,
        test_commands,
        languages,
        has_git,
        subprojects,
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
        f.write_all(b"fn main() {}").unwrap();

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
        f.write_all(b"print('hi')").unwrap();

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

    #[test]
    fn detects_mixed_rust_python_cpp_monorepo() {
        let dir = tempfile::tempdir().unwrap();
        // Rust component
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        // Python component
        std::fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();
        // C++ component
        std::fs::write(
            dir.path().join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.10)",
        )
        .unwrap();

        let profile = detect(dir.path()).unwrap();
        // Primary is Cargo (first in detection order).
        assert_eq!(profile.ecosystem, Ecosystem::Cargo);
        // All three ecosystems must be present.
        assert!(
            profile.ecosystems.contains(&Ecosystem::Cargo),
            "missing Cargo"
        );
        assert!(
            profile.ecosystems.contains(&Ecosystem::Python),
            "missing Python"
        );
        assert!(
            profile.ecosystems.contains(&Ecosystem::Cmake),
            "missing CMake"
        );
        // Per-ecosystem commands.
        assert!(profile.build_commands.contains_key("cargo"));
        assert!(profile.build_commands.contains_key("python"));
        assert!(profile.build_commands.contains_key("cmake"));
    }

    #[test]
    fn detects_dotnet_solution() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("MyApp.sln"),
            "Microsoft Visual Studio Solution",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("MyApp")).unwrap();
        std::fs::write(
            dir.path().join("MyApp").join("MyApp.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk\" />",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("MyApp").join("Program.cs"),
            "using System; Console.WriteLine(\"hi\");",
        )
        .unwrap();

        let profile = detect(dir.path()).unwrap();
        assert_eq!(profile.ecosystem, Ecosystem::Dotnet);
        assert_eq!(profile.build_command.as_deref(), Some("dotnet build"));
        assert!(profile.languages.contains(&"C#".to_string()));
    }

    #[test]
    fn detects_cpp_with_makefile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Makefile"), "all:\n\tg++ main.cpp -o app").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src").join("main.cpp"),
            "#include <iostream>\n",
        )
        .unwrap();

        let profile = detect(dir.path()).unwrap();
        assert_eq!(profile.ecosystem, Ecosystem::Cmake); // Make maps to Cmake
        assert!(profile.build_commands.contains_key("make"));
    }

    #[test]
    fn detects_node_with_pnpm() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - \"*\"",
        )
        .unwrap();

        let profile = detect(dir.path()).unwrap();
        assert_eq!(profile.ecosystem, Ecosystem::Node);
        // pnpm should be detected as the package manager.
        assert_eq!(
            profile.build_commands.get("node").map(String::as_str),
            Some("pnpm run build")
        );
    }

    #[test]
    fn discovers_subprojects_in_monorepo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("crates").join("core")).unwrap();
        std::fs::write(
            dir.path().join("crates").join("core").join("Cargo.toml"),
            "[package]\nname = \"core\"",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("python")).unwrap();
        std::fs::write(
            dir.path().join("python").join("pyproject.toml"),
            "[project]",
        )
        .unwrap();

        let profile = detect(dir.path()).unwrap();
        // Subprojects should include the nested crate and the python dir.
        assert!(
            profile.subprojects.iter().any(|s| s.contains("core")),
            "missing core subproject: {:?}",
            profile.subprojects
        );
        assert!(
            profile.subprojects.iter().any(|s| s.contains("python")),
            "missing python subproject: {:?}",
            profile.subprojects
        );
    }
}
