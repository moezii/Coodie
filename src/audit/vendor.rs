//! Vendored third-party code.
//!
//! A repository often carries someone else's project inside it: a C library
//! bundled in a `-sys` crate, a `third_party/` tree, a git submodule. That code
//! is accounted for by its upstream, not by this project. Its "dead" functions
//! are the upstream's public API, its tests belong to the upstream, and nothing
//! about it is actionable here. Auditing it as if it were the project's own
//! code made 93% of Reth's dead-code findings come from bundled libmdbx.
//!
//! Vendored directories are therefore **excluded** from the audit (and listed
//! in the report, so the exclusion is visible rather than silent). Treating
//! their symbols as public API instead would still count them in the
//! accounted-for percentage and in the untested public surface, which says
//! something about the upstream rather than about the code under audit.
//!
//! Three rules, each a statement the repository itself makes:
//!
//! 1. A path segment that by name holds third-party code: `vendor/`,
//!    `vendored/`, `third_party/`, `third-party/`, `thirdparty/`, `3rdparty/`,
//!    `external/`.
//! 2. A path listed as a git submodule in `.gitmodules`.
//! 3. A nested foreign project: a directory below the root that carries its
//!    own licence (one whose text differs from every licence at the root) and
//!    the build manifest of a *different* ecosystem than the nearest enclosing
//!    package, with no manifest of that package's own ecosystem. A C library
//!    with its own `LICENSE` and `CMakeLists.txt` inside a Rust crate is
//!    someone else's project; a workspace crate with its own `LICENSE` and
//!    `Cargo.toml` is not, and neither is the project's own docs site that
//!    carries a copy of the project's licence.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;

/// Directory names that hold third-party code by convention.
const VENDOR_SEGMENTS: &[&str] = &[
    "vendor",
    "vendored",
    "third_party",
    "third-party",
    "thirdparty",
    "3rdparty",
    "external",
];

/// The ecosystem a build manifest belongs to.
fn manifest_ecosystem(file_name: &str) -> Option<&'static str> {
    let lower = file_name.to_ascii_lowercase();
    let ecosystem = match lower.as_str() {
        "cargo.toml" => "rust",
        "package.json" => "javascript",
        "pyproject.toml" | "setup.py" | "setup.cfg" => "python",
        "go.mod" => "go",
        "pom.xml" | "build.gradle" | "build.gradle.kts" | "build.sbt" => "jvm",
        "composer.json" => "php",
        "gemfile" => "ruby",
        "mix.exs" => "elixir",
        "package.swift" => "swift",
        "cmakelists.txt" | "meson.build" | "configure.ac" | "configure.in" => "c",
        _ if lower.ends_with(".gemspec") => "ruby",
        _ if lower.ends_with(".csproj") || lower.ends_with(".fsproj") => "dotnet",
        _ => return None,
    };
    Some(ecosystem)
}

fn is_license_file(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    ["license", "licence", "copying", "copyright"]
        .iter()
        .any(|stem| {
            lower == *stem
                || lower.starts_with(&format!("{stem}."))
                || lower.starts_with(&format!("{stem}-"))
        })
}

/// What one directory says about itself.
#[derive(Default, Clone)]
struct DirFacts {
    /// Normalised text of each licence file in the directory.
    licenses: Vec<String>,
    ecosystems: BTreeSet<&'static str>,
}

fn dir_facts(root: &Path, relative_dir: &str, cache: &mut HashMap<String, DirFacts>) -> DirFacts {
    if let Some(found) = cache.get(relative_dir) {
        return found.clone();
    }
    let mut facts = DirFacts::default();
    let path = if relative_dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative_dir)
    };
    if let Ok(entries) = fs::read_dir(&path) {
        for entry in entries.flatten() {
            if !entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_license_file(&name) {
                let text = fs::read_to_string(entry.path()).unwrap_or_default();
                facts
                    .licenses
                    .push(text.split_whitespace().collect::<Vec<_>>().join(" "));
            }
            if let Some(ecosystem) = manifest_ecosystem(&name) {
                facts.ecosystems.insert(ecosystem);
            }
        }
    }
    cache.insert(relative_dir.to_owned(), facts.clone());
    facts
}

/// Submodule paths declared in `.gitmodules`, relative to the root.
fn submodule_paths(root: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(root.join(".gitmodules")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "path").then(|| value.trim().trim_matches('/').replace('\\', "/"))
        })
        .filter(|path| !path.is_empty())
        .collect()
}

/// The vendored directories among the given files, relative to `root`.
///
/// `relative_files` are `/`-separated paths relative to the root. The result
/// is sorted and holds only outermost directories.
pub fn vendored_directories(root: &Path, relative_files: &[String]) -> Vec<String> {
    let submodules = submodule_paths(root);
    let mut cache: HashMap<String, DirFacts> = HashMap::new();
    let mut verdicts: HashMap<String, Option<String>> = HashMap::new();
    let mut found: BTreeSet<String> = BTreeSet::new();

    for file in relative_files {
        let Some((dir, _)) = file.rsplit_once('/') else {
            continue;
        };
        if let Some(cached) = verdicts.get(dir) {
            if let Some(vendored) = cached {
                found.insert(vendored.clone());
            }
            continue;
        }
        let verdict = vendored_ancestor(root, dir, &submodules, &mut cache);
        if let Some(vendored) = &verdict {
            found.insert(vendored.clone());
        }
        verdicts.insert(dir.to_owned(), verdict);
    }

    // Keep only outermost directories.
    let mut outermost: Vec<String> = Vec::new();
    for dir in found {
        if !outermost
            .iter()
            .any(|kept| dir.starts_with(&format!("{kept}/")))
        {
            outermost.push(dir);
        }
    }
    outermost
}

/// The outermost ancestor of `dir` (inclusive) that is vendored, if any.
fn vendored_ancestor(
    root: &Path,
    dir: &str,
    submodules: &[String],
    cache: &mut HashMap<String, DirFacts>,
) -> Option<String> {
    let segments: Vec<&str> = dir.split('/').collect();
    // Ecosystems of the nearest enclosing package seen so far, walking down.
    let root_facts = dir_facts(root, "", cache);
    let mut enclosing: BTreeSet<&'static str> = root_facts.ecosystems;

    for depth in 1..=segments.len() {
        let prefix = segments[..depth].join("/");
        let name = segments[depth - 1].to_ascii_lowercase();

        if VENDOR_SEGMENTS.contains(&name.as_str()) || submodules.contains(&prefix) {
            return Some(prefix);
        }

        let facts = dir_facts(root, &prefix, cache);
        let own_license = facts
            .licenses
            .iter()
            .any(|text| !root_facts.licenses.contains(text));
        if own_license
            && !enclosing.is_empty()
            && !facts.ecosystems.is_empty()
            && facts.ecosystems.is_disjoint(&enclosing)
        {
            return Some(prefix);
        }
        if !facts.ecosystems.is_empty() {
            enclosing = facts.ecosystems;
        }
    }
    None
}

/// Is this relative path inside one of the vendored directories?
pub fn is_inside(path: &str, vendored: &[String]) -> bool {
    vendored
        .iter()
        .any(|dir| path.starts_with(dir.as_str()) && path[dir.len()..].starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(root: &Path, path: &str, text: &str) {
        let full = root.join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("dirs");
        fs::write(full, text).expect("write");
    }

    #[test]
    fn a_foreign_library_bundled_in_a_crate_is_vendored() {
        let directory = tempdir().expect("temp");
        let root = directory.path();
        write(root, "Cargo.toml", "[workspace]\n");
        write(root, "crates/sys/Cargo.toml", "[package]\n");
        write(root, "crates/sys/src/lib.rs", "");
        write(root, "crates/sys/clib/LICENSE", "");
        write(root, "crates/sys/clib/CMakeLists.txt", "");
        write(root, "crates/sys/clib/lib.c", "");
        write(root, "crates/sys/clib/sub/more.c", "");
        let files = vec![
            "crates/sys/src/lib.rs".to_owned(),
            "crates/sys/clib/lib.c".to_owned(),
            "crates/sys/clib/sub/more.c".to_owned(),
        ];
        assert_eq!(
            vendored_directories(root, &files),
            vec!["crates/sys/clib".to_owned()]
        );
    }

    #[test]
    fn a_workspace_member_with_its_own_license_is_not_vendored() {
        let directory = tempdir().expect("temp");
        let root = directory.path();
        write(root, "Cargo.toml", "[workspace]\n");
        write(root, "crates/a/Cargo.toml", "[package]\n");
        write(root, "crates/a/LICENSE-MIT", "");
        write(root, "crates/a/src/lib.rs", "");
        // A licence alone, with no foreign build, is not a foreign project.
        write(root, "crates/a/src/data/LICENSE", "");
        write(root, "crates/a/src/data/gen.rs", "");
        let files = vec![
            "crates/a/src/lib.rs".to_owned(),
            "crates/a/src/data/gen.rs".to_owned(),
        ];
        assert!(vendored_directories(root, &files).is_empty());
    }

    #[test]
    fn a_subproject_carrying_the_projects_own_licence_is_not_vendored() {
        // The project's own docs site: another ecosystem, but the same licence.
        let directory = tempdir().expect("temp");
        let root = directory.path();
        write(
            root,
            "Cargo.toml",
            "[workspace]
",
        );
        write(
            root,
            "LICENSE",
            "MIT License
Copyright (c) us
",
        );
        write(root, "docs/site/package.json", "{}");
        write(
            root,
            "docs/site/LICENSE",
            "MIT License
  Copyright (c) us
",
        );
        write(root, "docs/site/index.ts", "");
        let files = vec!["docs/site/index.ts".to_owned()];
        assert!(vendored_directories(root, &files).is_empty());
    }

    #[test]
    fn conventional_names_and_submodules_are_vendored() {
        let directory = tempdir().expect("temp");
        let root = directory.path();
        write(
            root,
            ".gitmodules",
            "[submodule \"deps/lib\"]\n\tpath = deps/lib\n\turl = x\n",
        );
        let files = vec![
            "src/third_party/zlib/inflate.c".to_owned(),
            "deps/lib/a.c".to_owned(),
            "src/main.rs".to_owned(),
        ];
        assert_eq!(
            vendored_directories(root, &files),
            vec!["deps/lib".to_owned(), "src/third_party".to_owned()]
        );
        assert!(is_inside("deps/lib/a.c", &["deps/lib".to_owned()]));
        assert!(!is_inside("deps/library/a.c", &["deps/lib".to_owned()]));
    }
}
