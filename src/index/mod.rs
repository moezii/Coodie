mod git;
mod grammar;
mod language;
mod symbols;
mod walker;
mod watch;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use thiserror::Error;
use tree_sitter::Parser;

use crate::cache::{hash_bytes, hash_parts, CacheError, CacheStore};
use git::GitRepository;

pub use git::Provenance;
pub use grammar::{
    extract_calls, extract_import_aliases, extract_symbols, grammar_for, split_cpp_qualified_name,
    Grammar, RawCall,
};
pub use language::{language_specs, SourceLanguage};
pub(crate) use language::strip_comments_and_strings;
pub use symbols::{ByteRange, LineRange, Symbol, SymbolKind};
pub use walker::{discover_files, discover_source_files, SourceFile, WalkedFiles};
pub use watch::watch_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageStats {
    pub files_parsed: usize,
    pub functions_found: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSummary {
    root: PathBuf,
    files_parsed: usize,
    functions_found: usize,
    language_stats: BTreeMap<SourceLanguage, LanguageStats>,
    symbols: Vec<Symbol>,
    ast_cache_hits: usize,
    ast_cache_misses: usize,
    files_reparsed: usize,
    previous_head: Option<String>,
    current_head: Option<String>,
    changed_source_files: usize,
    dirty_source_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUpdateSummary {
    pub path: PathBuf,
    pub functions_found: usize,
    pub cache_hit: bool,
    pub reparsed: bool,
}

impl IndexSummary {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn files_parsed(&self) -> usize {
        self.files_parsed
    }

    pub fn functions_found(&self) -> usize {
        self.functions_found
    }

    pub fn language_stats(&self) -> &BTreeMap<SourceLanguage, LanguageStats> {
        &self.language_stats
    }

    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    pub fn ast_cache_hits(&self) -> usize {
        self.ast_cache_hits
    }

    pub fn ast_cache_misses(&self) -> usize {
        self.ast_cache_misses
    }

    pub fn files_reparsed(&self) -> usize {
        self.files_reparsed
    }

    pub fn previous_head(&self) -> Option<&str> {
        self.previous_head.as_deref()
    }

    pub fn current_head(&self) -> Option<&str> {
        self.current_head.as_deref()
    }

    pub fn changed_source_files(&self) -> usize {
        self.changed_source_files
    }

    pub fn dirty_source_files(&self) -> usize {
        self.dirty_source_files
    }
}

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("index path does not exist: {path}")]
    PathNotFound { path: PathBuf },
    #[error("failed to canonicalize index path {path}: {source}")]
    CanonicalizePath {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to walk source tree: {0}")]
    Walk(#[from] ignore::Error),
    #[error("failed to read source file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to configure parser for {language}: {message}")]
    ParserLanguage {
        language: SourceLanguage,
        message: String,
    },
    #[error("tree-sitter did not return a parse tree for {path}")]
    ParseReturnedNoTree { path: PathBuf },
    #[error("failed to run git executable {executable}: {source}")]
    GitIo {
        executable: PathBuf,
        source: std::io::Error,
    },
    #[error("git command failed: {executable} {args:?}, status {status}, stderr: {stderr}")]
    GitFailed {
        executable: PathBuf,
        args: Vec<String>,
        status: ExitStatus,
        stderr: String,
    },
    #[error("file watcher error: {0}")]
    Notify(#[from] notify::Error),
    #[error("file watcher channel closed")]
    WatchChannelClosed,
    #[error("failed to write watcher output: {0}")]
    WatchOutput(std::io::Error),
    #[error(transparent)]
    Cache(#[from] CacheError),
}

pub fn index_path(path: impl AsRef<Path>) -> Result<IndexSummary, IndexError> {
    let requested_root = path.as_ref();
    if !requested_root.exists() {
        return Err(IndexError::PathNotFound {
            path: requested_root.to_path_buf(),
        });
    }

    let root = requested_root
        .canonicalize()
        .map_err(|source| IndexError::CanonicalizePath {
            path: requested_root.to_path_buf(),
            source,
        })?;

    let mut summary = IndexSummary {
        root: root.clone(),
        files_parsed: 0,
        functions_found: 0,
        language_stats: BTreeMap::new(),
        symbols: Vec::new(),
        ast_cache_hits: 0,
        ast_cache_misses: 0,
        files_reparsed: 0,
        previous_head: None,
        current_head: None,
        changed_source_files: 0,
        dirty_source_files: 0,
    };
    let cache = CacheStore::open_for_root(&root)?;
    let lifecycle = IndexLifecycle::load(&root, &cache)?;
    summary.previous_head = lifecycle.previous_head.clone();
    summary.current_head = lifecycle.current_head.clone();
    summary.changed_source_files = lifecycle.changed_source_files.len();
    summary.dirty_source_files = lifecycle.dirty_source_files.len();

    for source_file in lifecycle.source_files {
        let file_index = index_source_file_with_cache(&cache, &source_file)?;
        summary.ast_cache_hits += usize::from(file_index.cache_hit);
        summary.ast_cache_misses += usize::from(!file_index.cache_hit);
        summary.files_reparsed += usize::from(file_index.reparsed);
        summary.files_parsed += 1;
        summary.functions_found += file_index.symbols.len();

        let stats = summary
            .language_stats
            .entry(source_file.language.clone())
            .or_insert(LanguageStats {
                files_parsed: 0,
                functions_found: 0,
            });
        stats.files_parsed += 1;
        stats.functions_found += file_index.symbols.len();

        summary.symbols.extend(file_index.symbols);
    }

    summary.symbols.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line_range.start.cmp(&right.line_range.start))
            .then(left.name.cmp(&right.name))
    });

    if let Some(current_head) = &summary.current_head {
        cache.set_last_indexed_head(current_head)?;
    }

    Ok(summary)
}

#[derive(Debug)]
struct IndexLifecycle {
    source_files: Vec<SourceFile>,
    previous_head: Option<String>,
    current_head: Option<String>,
    changed_source_files: BTreeSet<PathBuf>,
    dirty_source_files: BTreeSet<PathBuf>,
}

impl IndexLifecycle {
    fn load(root: &Path, cache: &CacheStore) -> Result<Self, IndexError> {
        let source_files = discover_source_files(root)?;
        let source_paths = source_files
            .iter()
            .filter_map(|source_file| relative_source_path(root, &source_file.path))
            .collect::<HashSet<_>>();
        let repository = GitRepository::discover(root)?;
        let previous_head = cache.last_indexed_head()?;
        let current_head = repository
            .as_ref()
            .map(GitRepository::head_sha)
            .transpose()?
            .flatten();

        let mut changed_source_files = BTreeSet::new();
        let mut dirty_source_files = BTreeSet::new();

        if let Some(repository) = &repository {
            if let (Some(previous), Some(current)) = (&previous_head, &current_head) {
                if previous != current {
                    for path in repository.diff_paths(previous, current)? {
                        if source_paths.contains(&path) {
                            changed_source_files.insert(path);
                        }
                    }
                }
            } else {
                changed_source_files.extend(source_paths.iter().cloned());
            }

            for path in repository.dirty_paths()? {
                if source_paths.contains(&path) {
                    dirty_source_files.insert(path);
                }
            }
        } else {
            changed_source_files.extend(source_paths.iter().cloned());
        }

        Ok(Self {
            source_files,
            previous_head,
            current_head,
            changed_source_files,
            dirty_source_files,
        })
    }
}

#[derive(Debug)]
struct FileIndexResult {
    symbols: Vec<Symbol>,
    cache_hit: bool,
    reparsed: bool,
}

fn index_source_file_with_cache(
    cache: &CacheStore,
    source_file: &SourceFile,
) -> Result<FileIndexResult, IndexError> {
    let source = fs::read(&source_file.path).map_err(|source| IndexError::ReadFile {
        path: source_file.path.clone(),
        source,
    })?;
    let content_hash = hash_parts(&[b"grammar-query-v1", &source]);

    if let Some(mut symbols) = cache.get_ast_symbols(&content_hash, &source_file.language)? {
        cache.record_ast_hit()?;
        for symbol in &mut symbols {
            symbol.file = source_file.path.clone();
        }
        return Ok(FileIndexResult {
            symbols,
            cache_hit: true,
            reparsed: false,
        });
    }

    cache.record_ast_miss()?;
    let symbols = parse_source_symbols(source_file.language.clone(), &source_file.path, &source)?;
    cache.put_ast_symbols(&content_hash, &source_file.language, source.len(), &symbols)?;
    Ok(FileIndexResult {
        symbols,
        cache_hit: false,
        reparsed: true,
    })
}

fn relative_source_path(root: &Path, path: &Path) -> Option<PathBuf> {
    path.strip_prefix(root)
        .ok()
        .map(|path| PathBuf::from(path.to_string_lossy().replace('\\', "/")))
}

pub fn source_state_hash(path: impl AsRef<Path>) -> Result<String, IndexError> {
    let requested_root = path.as_ref();
    let root = requested_root
        .canonicalize()
        .map_err(|source| IndexError::CanonicalizePath {
            path: requested_root.to_path_buf(),
            source,
        })?;
    let mut state = Vec::new();

    for source_file in discover_source_files(&root)? {
        let source = fs::read(&source_file.path).map_err(|source| IndexError::ReadFile {
            path: source_file.path.clone(),
            source,
        })?;
        let relative_path = source_file
            .path
            .strip_prefix(&root)
            .unwrap_or(&source_file.path);
        let relative_path = relative_path.to_string_lossy();
        let content_hash = hash_bytes(&source);

        state.extend_from_slice(relative_path.as_bytes());
        state.push(0);
        state.extend_from_slice(source_file.language.cache_label().as_bytes());
        state.push(0);
        state.extend_from_slice(content_hash.as_bytes());
        state.push(b'\n');
    }

    Ok(hash_bytes(&state))
}

pub fn reindex_changed_file(
    root: impl AsRef<Path>,
    path: impl AsRef<Path>,
) -> Result<Option<FileUpdateSummary>, IndexError> {
    let requested_root = root.as_ref();
    let root = requested_root
        .canonicalize()
        .map_err(|source| IndexError::CanonicalizePath {
            path: requested_root.to_path_buf(),
            source,
        })?;
    let requested_path = path.as_ref();
    let path = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        root.join(requested_path)
    };

    if !path.is_file() {
        return Ok(None);
    }

    let source = fs::read(&path).map_err(|source| IndexError::ReadFile {
        path: path.clone(),
        source,
    })?;
    let Some(language) = SourceLanguage::from_path_and_source(&path, &source) else {
        return Ok(None);
    };
    // One file on its own: only the header's content can say it is C++.
    let language = language.refine_header(&path, &source, false);

    let cache = CacheStore::open_for_root(&root)?;
    let source_file = SourceFile {
        path: path
            .canonicalize()
            .map_err(|source| IndexError::CanonicalizePath {
                path: path.clone(),
                source,
            })?,
        language,
    };
    let indexed = index_source_file_with_cache(&cache, &source_file)?;

    Ok(Some(FileUpdateSummary {
        path: source_file.path,
        functions_found: indexed.symbols.len(),
        cache_hit: indexed.cache_hit,
        reparsed: indexed.reparsed,
    }))
}

/// Last-edit provenance for a span of a file, or `None` outside git / for uncommitted spans.
pub fn last_editor(
    root: &Path,
    file: &Path,
    line_range: &LineRange,
) -> Result<Option<Provenance>, IndexError> {
    let Some(repository) = GitRepository::discover(root)? else {
        return Ok(None);
    };
    repository.last_edit(file, line_range.start, line_range.end)
}

pub fn parse_source_symbols(
    language: SourceLanguage,
    path: &Path,
    source: &[u8],
) -> Result<Vec<Symbol>, IndexError> {
    let mut parser = Parser::new();
    let Some(grammar) = grammar::grammar_for(&language) else {
        return Ok(Vec::new());
    };

    parser
        .set_language(&grammar.language())
        .map_err(|error| IndexError::ParserLanguage {
            language: language.clone(),
            message: error.to_string(),
        })?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| IndexError::ParseReturnedNoTree {
            path: path.to_path_buf(),
        })?;

    Ok(grammar::extract_symbols(
        grammar,
        language,
        path,
        source,
        tree.root_node(),
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{git, index_path, parse_source_symbols, SourceLanguage, SymbolKind};

    #[test]
    fn extracts_python_function_names_ranges_and_scope() {
        let source = br#"
class SessionService:
    def login(self, user):
        return user

def verify_token(token):
    return token
"#;

        let symbols = parse_source_symbols(
            SourceLanguage::new("python").unwrap(),
            "service.py".as_ref(),
            source,
        )
        .unwrap();
        let names = symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();

        // The class is findable in its own right, not only a scope label.
        assert_eq!(names, ["SessionService", "login", "verify_token"]);
        assert_eq!(
            symbols
                .iter()
                .find(|symbol| symbol.name == "SessionService")
                .map(|symbol| symbol.kind),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            symbols
                .iter()
                .find(|symbol| symbol.name == "login")
                .map(|symbol| symbol.kind),
            Some(SymbolKind::Function)
        );

        let login = symbols
            .iter()
            .find(|symbol| symbol.name == "login")
            .unwrap();
        assert_eq!(login.enclosing_scope, "SessionService");
        assert_eq!(login.line_range.start, 3);
        assert!(login.byte_range.start < login.byte_range.end);

        let verify = symbols
            .iter()
            .find(|symbol| symbol.name == "verify_token")
            .unwrap();
        assert_eq!(verify.enclosing_scope, "module");
        assert_eq!(verify.line_range.start, 6);
    }

    #[test]
    fn extracts_javascript_and_typescript_functions() {
        let javascript = br#"
function fetchUser() {
  return true;
}

class ApiClient {
  postData() {
    return fetch("/users");
  }
}

const authenticate = () => true;
"#;
        let js_symbols = parse_source_symbols(
            SourceLanguage::new("javascript").unwrap(),
            "api.js".as_ref(),
            javascript,
        )
        .unwrap();
        let js_names = js_symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            js_names,
            ["fetchUser", "ApiClient", "postData", "authenticate"]
        );
        assert_eq!(
            js_symbols
                .iter()
                .find(|symbol| symbol.name == "ApiClient")
                .map(|symbol| symbol.kind),
            Some(SymbolKind::Class)
        );
        let method = js_symbols
            .iter()
            .find(|symbol| symbol.name == "postData")
            .unwrap();
        assert_eq!(method.enclosing_scope, "ApiClient");

        let typescript = br#"
export function verifyToken(token: string): boolean {
  return token.length > 0;
}

const refreshSession = function (): boolean {
  return true;
};
"#;
        let ts_symbols = parse_source_symbols(
            SourceLanguage::new("typescript").unwrap(),
            "auth.ts".as_ref(),
            typescript,
        )
        .unwrap();
        let ts_names = ts_symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ts_names, ["verifyToken", "refreshSession"]);
    }

    #[test]
    fn index_path_respects_gitignore_and_non_source_files() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join(".gitignore"), "ignored.py\n").unwrap();
        fs::write(
            directory.path().join("kept.py"),
            "def visible_function():\n    return True\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("ignored.py"),
            "def ignored_function():\n    return False\n",
        )
        .unwrap();
        fs::write(directory.path().join("notes.txt"), "def not_code(): pass\n").unwrap();

        let summary = index_path(directory.path()).unwrap();

        assert_eq!(summary.files_parsed(), 1);
        assert_eq!(summary.functions_found(), 1);
        assert_eq!(summary.symbols()[0].name, "visible_function");
    }

    #[test]
    fn indexes_query_backed_modern_language_symbols_as_complete_functions() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("main.rs"),
            "pub fn authenticate(user: &str) -> bool {\n    let normalized = user.trim();\n    !normalized.is_empty()\n}\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("service.go"),
            "package main\n\nfunc VerifyToken(token string) bool {\n    cleaned := strings.TrimSpace(token)\n    return cleaned != \"\"\n}\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("AuthService.java"),
            "class AuthService {\n  boolean checkPassword(String pw) {\n    return pw != null && !pw.isEmpty();\n  }\n}\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("Util.php"),
            r#"<?php
class Util {
    public function normalize($value) {
        return trim($value);
    }
}
"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("analysis.R"),
            "clean_value <- function(value) {\n    trimmed <- trimws(value)\n    trimmed\n}\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("Service.scala"),
            "class Service {\n  def checkName(name: String): Boolean = {\n    name.trim.nonEmpty\n  }\n}\n",
        )
        .unwrap();

        let summary = index_path(directory.path()).unwrap();
        let mut names = summary
            .symbols()
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();

        assert!(names.contains(&"authenticate"));
        assert!(names.contains(&"VerifyToken"));
        assert!(names.contains(&"checkPassword"));
        assert!(names.contains(&"normalize"));
        assert!(names.contains(&"clean_value"));
        assert!(names.contains(&"checkName"));
        assert_complete_symbol_snippet(summary.symbols(), "authenticate", "let normalized");
        assert_complete_symbol_snippet(summary.symbols(), "VerifyToken", "return cleaned");
        assert_complete_symbol_snippet(summary.symbols(), "checkPassword", "return pw != null");
        assert_complete_symbol_snippet(summary.symbols(), "normalize", "return trim");
        assert_complete_symbol_snippet(summary.symbols(), "clean_value", "trimmed <- trimws");
        assert_complete_symbol_snippet(summary.symbols(), "checkName", "name.trim.nonEmpty");
        assert!(summary
            .language_stats()
            .keys()
            .any(|language| language.cache_label() == "rust"));
        assert!(summary
            .language_stats()
            .keys()
            .any(|language| language.cache_label() == "go"));
        assert!(summary
            .language_stats()
            .keys()
            .any(|language| language.cache_label() == "java"));
        assert!(summary
            .language_stats()
            .keys()
            .any(|language| language.cache_label() == "php"));
        assert!(summary
            .language_stats()
            .keys()
            .any(|language| language.cache_label() == "r"));
        assert!(summary
            .language_stats()
            .keys()
            .any(|language| language.cache_label() == "scala"));
    }

    fn assert_complete_symbol_snippet(symbols: &[super::Symbol], name: &str, body_text: &str) {
        let symbol = symbols
            .iter()
            .find(|symbol| symbol.name == name)
            .unwrap_or_else(|| panic!("missing symbol {name}"));
        let source = fs::read(&symbol.file).unwrap();
        let snippet =
            String::from_utf8_lossy(&source[symbol.byte_range.start..symbol.byte_range.end]);

        assert!(
            snippet.contains(body_text),
            "{name} snippet was only {snippet:?}"
        );
        assert!(symbol.line_range.end > symbol.line_range.start);
    }

    #[test]
    fn index_path_uses_ast_cache_for_unchanged_content() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("auth.py"),
            "def verify_token(token):\n    return token\n",
        )
        .unwrap();

        let first = index_path(directory.path()).unwrap();
        let second = index_path(directory.path()).unwrap();

        assert_eq!(first.files_reparsed(), 1);
        assert_eq!(first.ast_cache_misses(), 1);
        assert_eq!(first.ast_cache_hits(), 0);
        assert_eq!(second.files_parsed(), 1);
        assert_eq!(second.files_reparsed(), 0);
        assert_eq!(second.ast_cache_hits(), 1);
        assert_eq!(second.ast_cache_misses(), 0);
        assert_eq!(second.symbols()[0].name, "verify_token");
    }

    #[test]
    fn commit_to_commit_diff_reparses_only_changed_source_files() {
        let directory = tempdir().unwrap();
        init_git_repo(directory.path());
        fs::write(
            directory.path().join("a.py"),
            "def alpha():\n    return 1\n",
        )
        .unwrap();
        fs::write(directory.path().join("b.py"), "def beta():\n    return 2\n").unwrap();
        commit_all(directory.path(), "initial");
        let first_head = git_head(directory.path());

        let first = index_path(directory.path()).unwrap();
        fs::write(
            directory.path().join("a.py"),
            "def alpha():\n    return 10\n",
        )
        .unwrap();
        commit_all(directory.path(), "change alpha");
        let second_head = git_head(directory.path());
        let second = index_path(directory.path()).unwrap();

        assert_eq!(first.files_reparsed(), 2);
        assert_eq!(second.previous_head(), Some(first_head.as_str()));
        assert_eq!(second.current_head(), Some(second_head.as_str()));
        assert_eq!(second.changed_source_files(), 1);
        assert_eq!(second.dirty_source_files(), 0);
        assert_eq!(second.files_reparsed(), 1);
        assert_eq!(second.ast_cache_hits(), 1);
    }

    #[test]
    fn dirty_uncommitted_edit_is_detected_and_reindexed() {
        let directory = tempdir().unwrap();
        init_git_repo(directory.path());
        fs::write(
            directory.path().join("auth.py"),
            "def verify_token(token):\n    return token\n",
        )
        .unwrap();
        commit_all(directory.path(), "initial");
        let head = git_head(directory.path());

        let first = index_path(directory.path()).unwrap();
        fs::write(
            directory.path().join("auth.py"),
            "def verify_token(token):\n    return token.strip()\n",
        )
        .unwrap();
        let second = index_path(directory.path()).unwrap();

        assert_eq!(first.files_reparsed(), 1);
        assert_eq!(second.previous_head(), Some(head.as_str()));
        assert_eq!(second.current_head(), Some(head.as_str()));
        assert_eq!(second.changed_source_files(), 0);
        assert_eq!(second.dirty_source_files(), 1);
        assert_eq!(second.files_reparsed(), 1);
        assert_eq!(second.symbols()[0].name, "verify_token");
    }

    #[test]
    fn revisiting_previously_seen_content_state_is_cache_hit() {
        let directory = tempdir().unwrap();
        init_git_repo(directory.path());
        fs::write(
            directory.path().join("state.py"),
            "def value():\n    return 1\n",
        )
        .unwrap();
        commit_all(directory.path(), "state one");
        let first_head = git_head(directory.path());

        let first = index_path(directory.path()).unwrap();
        fs::write(
            directory.path().join("state.py"),
            "def value():\n    return 2\n",
        )
        .unwrap();
        commit_all(directory.path(), "state two");
        let second = index_path(directory.path()).unwrap();

        run_git(
            directory.path(),
            ["checkout", "--detach", first_head.as_str()],
        );
        let revisited = index_path(directory.path()).unwrap();

        assert_eq!(first.files_reparsed(), 1);
        assert_eq!(second.files_reparsed(), 1);
        assert_eq!(revisited.current_head(), Some(first_head.as_str()));
        assert_eq!(revisited.changed_source_files(), 1);
        assert_eq!(revisited.files_reparsed(), 0);
        assert_eq!(revisited.ast_cache_hits(), 1);
    }

    fn init_git_repo(path: &Path) {
        run_git(path, ["init"]);
        fs::write(path.join(".gitignore"), ".codereader/\n").unwrap();
        run_git(path, ["config", "user.email", "codereader@example.invalid"]);
        run_git(path, ["config", "user.name", "Code Reader Test"]);
        run_git(path, ["config", "core.autocrlf", "false"]);
    }

    fn commit_all(path: &Path, message: &str) {
        run_git(path, ["add", "."]);
        run_git(path, ["commit", "--no-gpg-sign", "-m", message]);
    }

    fn git_head(path: &Path) -> String {
        let output = run_git(path, ["rev-parse", "HEAD"]);
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn run_git<const N: usize>(path: &Path, args: [&str; N]) -> std::process::Output {
        let git =
            git::discover_git_executable().expect("git executable is required for lifecycle tests");
        let output = Command::new(git)
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("git command starts");

        assert!(
            output.status.success(),
            "git command failed: {}\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        output
    }
}
