//! The last check before anything is called dead.
//!
//! Every other stage of the audit depends on a language's extractor having
//! turned a use into a call edge or a recorded mention. Where nobody wrote that
//! rule — a C macro, a template, FFI, reflection, a name dispatched from a
//! string, a Makefile target, a YAML config, a shell script, another language
//! calling in — the use is invisible and the symbol looks dead. Design rule 1
//! says that must never become an accusation, so this module asks one
//! language-independent question of every symbol that is about to be reported
//! dead: **does its name appear anywhere else in the repository?**
//!
//! If it does, the symbol is *mentioned but not traced*: still listed, as
//! unresolved, never as dead. Only a name that occurs nowhere outside its own
//! definition stays dead.
//!
//! # What counts as a mention
//!
//! A whole-word occurrence of the symbol's name. Words are maximal runs of
//! ASCII letters, digits, `_`, and any non-ASCII byte; everything else (`.`,
//! `:`, `-`, `$`, quotes, …) is a boundary. So `foo` is mentioned by
//! `obj.foo()`, `"foo"`, `$(foo)`, `foo-bar` and `FOO=foo`, but not by
//! `foobar` or `my_foo`. Case matters. Comments and strings count: from raw
//! text there is no telling a comment from a macro body or a string a
//! dispatcher reads.
//!
//! For a qualified or decorated name (`Type::method`, `~Widget`,
//! `operator==`) the last word is looked up, which can only match more often.
//!
//! # Which files are read
//!
//! Every file the walker yields — every language, plus build files, configs,
//! scripts, templates and data — after the ignore rules and the vendored
//! exclusion have been applied. Two kinds are skipped:
//!
//! - **Prose documentation** (`*.md`, `*.markdown`, `*.rst`, `*.adoc`,
//!   `*.asciidoc`, `*.org`, `*.tex`, and changelog/licence/authors files). A
//!   function named in a README or a changelog entry is described, not used;
//!   counting those would let a single "removed `foo`" note hide dead code
//!   forever. Code files that happen to live under `docs/` are still read,
//!   because an example program there really does call the API. `*.mdx` is
//!   read: it imports and renders components.
//! - **Binary and huge files**: a NUL byte in the first 8 KiB, or more than
//!   [`MAX_FILE_BYTES`]. Neither holds a name a person wrote.
//!
//! # The definition span
//!
//! A symbol's own declaration line(s) and body, as the parser reported them,
//! are not a use of it — recursion is not a use. Everything else is: another
//! file, another line of the same file, and the definition of a *different*
//! symbol with the same name. That last point makes overloads and duplicates
//! block each other, which is deliberately conservative: from text alone a
//! second definition cannot be told from a separate prototype, a forward
//! declaration, or an overload the language dispatches to.
//!
//! The one exception is where a language extractor *knows* an occurrence is a
//! declaration: C and C++ report every place a function's name is declared
//! (prototypes in headers, out-of-class definitions, in-class method
//! declarations) as `FileFacts::declaration_sites`, and exactly that many
//! occurrences on that line are not counted. Without it every C/C++ function
//! with a header prototype would be excused, and a header is not a caller.
//!
//! # Cost
//!
//! The scan runs only when something would be reported dead, looks up only
//! those names, and reads files on every core. It is not cached: which names
//! it looks for changes with every edit anywhere in the repository, and
//! re-reading the files (already in the OS cache from the parse stage) is
//! cheaper than storing a full identifier index per file.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::parallel;

/// Files larger than this are not scanned. Generated blobs and data dumps are
/// not where a person refers to a function by name.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// How much of a file is checked for a NUL byte to decide it is binary.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// A symbol about to be reported dead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub node_id: String,
    pub name: String,
    /// Repository-relative path of the definition.
    pub file: String,
    /// The definition span, 1-based and inclusive.
    pub start_line: usize,
    pub end_line: usize,
}

/// Where a would-be-dead symbol's name was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntracedMention {
    pub node_id: String,
    /// The first mention outside the definition span, in walk order, as
    /// `path:line`. `None` when the name has no word to look up, which is
    /// treated as mentioned because it cannot be shown not to be.
    pub first_mention: Option<String>,
}

/// One file to scan, with the path the report should use for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFile {
    pub path: PathBuf,
    pub relative: String,
}

/// Is this a prose document, whose mentions of a name are not uses?
pub fn is_prose(relative: &str) -> bool {
    let lower = relative.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    const PROSE_EXTENSIONS: &[&str] = &[
        ".md", ".markdown", ".rst", ".adoc", ".asciidoc", ".org", ".tex",
    ];
    const PROSE_STEMS: &[&str] = &[
        "changelog", "changes", "history", "news", "license", "licence", "copying", "authors",
        "contributors", "notice", "codeowners",
    ];
    if PROSE_EXTENSIONS.iter().any(|extension| name.ends_with(extension)) {
        return true;
    }
    let stem = name.split('.').next().unwrap_or(name);
    PROSE_STEMS
        .iter()
        .any(|prose| stem == *prose || stem.starts_with(&format!("{prose}-")))
}

/// The word a name is looked up by: its last identifier run.
fn lookup_word(name: &str) -> Option<&str> {
    name.as_bytes()
        .split(|byte| !is_word_byte(*byte))
        .rev()
        .find(|word| !word.is_empty())
        // Splitting on ASCII bytes keeps every piece valid UTF-8.
        .and_then(|word| std::str::from_utf8(word).ok())
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Find which candidates are named somewhere outside their own definition.
///
/// Returns one entry per mentioned candidate, in candidate order. Candidates
/// absent from the result occur nowhere else and are genuinely unreferenced as
/// far as any text in the repository can tell.
///
/// `declarations` maps a file to `(name, line)` pairs a language extractor
/// knows to be declarations of that name rather than uses (C/C++ prototypes
/// and definitions). That many occurrences of the name on that line are not
/// counted. Everything else in every file is.
pub fn find_untraced(
    files: &[TextFile],
    candidates: &[Candidate],
    declarations: &HashMap<String, Vec<(String, usize)>>,
    threads: usize,
) -> Vec<UntracedMention> {
    if candidates.is_empty() {
        return Vec::new();
    }

    // Distinct lookup words, each with the candidates that share it.
    let mut words: HashMap<&str, usize> = HashMap::new();
    let mut word_list: Vec<&str> = Vec::new();
    let mut candidate_word: Vec<Option<usize>> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let index = lookup_word(&candidate.name).map(|word| {
            *words.entry(word).or_insert_with(|| {
                word_list.push(word);
                word_list.len() - 1
            })
        });
        candidate_word.push(index);
    }
    let shortest = word_list.iter().map(|word| word.len()).min().unwrap_or(0);
    let longest = word_list.iter().map(|word| word.len()).max().unwrap_or(0);

    // Per file, every line each looked-up word occurs on, ascending.
    let hits: Vec<Vec<(usize, usize)>> = parallel::map(files, threads, |file| {
        let Some(bytes) = read_text(file) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut line = 1usize;
        let mut index = 0usize;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte == b'\n' {
                line += 1;
                index += 1;
                continue;
            }
            if !is_word_byte(byte) {
                index += 1;
                continue;
            }
            let start = index;
            while index < bytes.len() && is_word_byte(bytes[index]) {
                index += 1;
            }
            let length = index - start;
            if length < shortest || length > longest {
                continue;
            }
            if let Ok(word) = std::str::from_utf8(&bytes[start..index]) {
                if let Some(&word_index) = words.get(word) {
                    found.push((word_index, line));
                }
            }
        }
        found
    });

    let file_index: HashMap<&str, usize> = files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.relative.as_str(), index))
        .collect();

    // Known declarations of a looked-up word: (file, line, word) -> how many.
    let mut declared: HashMap<(usize, usize, usize), usize> = HashMap::new();
    for (relative, sites) in declarations {
        let Some(&file) = file_index.get(relative.as_str()) else {
            continue;
        };
        for (name, line) in sites {
            if let Some(word) = lookup_word(name).and_then(|word| words.get(word)) {
                *declared.entry((file, *line, *word)).or_insert(0) += 1;
            }
        }
    }

    // Group by word: (file index, line), in walk order then line order,
    // dropping occurrences that are known declarations.
    let mut by_word: Vec<Vec<(usize, usize)>> = vec![Vec::new(); word_list.len()];
    for (file_index, found) in hits.iter().enumerate() {
        for &(word_index, line) in found {
            if let Some(remaining) = declared.get_mut(&(file_index, line, word_index)) {
                if *remaining > 0 {
                    *remaining -= 1;
                    continue;
                }
            }
            by_word[word_index].push((file_index, line));
        }
    }

    let mut mentioned = Vec::new();
    for (candidate, word) in candidates.iter().zip(&candidate_word) {
        let Some(word) = word else {
            mentioned.push(UntracedMention {
                node_id: candidate.node_id.clone(),
                first_mention: None,
            });
            continue;
        };
        let own_file = file_index.get(candidate.file.as_str()).copied();
        let outside = by_word[*word].iter().find(|(file, line)| {
            Some(*file) != own_file || *line < candidate.start_line || *line > candidate.end_line
        });
        if let Some((file, line)) = outside {
            mentioned.push(UntracedMention {
                node_id: candidate.node_id.clone(),
                first_mention: Some(format!("{}:{}", files[*file].relative, line)),
            });
        }
    }
    mentioned
}

/// A file's bytes, or `None` if it is binary, too large, or unreadable.
fn read_text(file: &TextFile) -> Option<Vec<u8>> {
    let metadata = fs::metadata(&file.path).ok()?;
    if metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = fs::read(&file.path).ok()?;
    let sniff = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    if sniff.contains(&0) {
        return None;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn candidate(name: &str, file: &str, start_line: usize, end_line: usize) -> Candidate {
        Candidate {
            node_id: format!("function:{file}:{name}:{start_line}"),
            name: name.to_owned(),
            file: file.to_owned(),
            start_line,
            end_line,
        }
    }

    fn scan(files: &[(&str, &[u8])], candidates: &[Candidate]) -> Vec<UntracedMention> {
        let directory = tempdir().expect("temp dir");
        let mut text_files = Vec::new();
        for (relative, content) in files {
            let path = directory.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("dirs");
            }
            fs::write(&path, content).expect("write");
            text_files.push(TextFile {
                path,
                relative: (*relative).to_owned(),
            });
        }
        find_untraced(&text_files, candidates, &HashMap::new(), 2)
    }

    #[test]
    fn a_name_used_only_inside_its_own_body_is_not_mentioned() {
        // Recursion is not a use.
        let source = b"fn countdown(n: u32) {\n    if n > 0 { countdown(n - 1) }\n}\n";
        let found = scan(&[("lib.rs", source)], &[candidate("countdown", "lib.rs", 1, 3)]);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_name_elsewhere_in_the_same_file_is_a_mention() {
        let source = b"fn helper() {}\n\nmacro_rules! go { () => { helper() } }\n";
        let found = scan(&[("lib.rs", source)], &[candidate("helper", "lib.rs", 1, 1)]);
        assert_eq!(found[0].first_mention.as_deref(), Some("lib.rs:3"));
    }

    #[test]
    fn a_name_in_a_makefile_is_a_mention() {
        let found = scan(
            &[
                ("tool.c", b"void build_tables(void) {\n}\n"),
                ("Makefile", b"tables:\n\t./tool --run=build_tables\n"),
            ],
            &[candidate("build_tables", "tool.c", 1, 2)],
        );
        assert_eq!(found[0].first_mention.as_deref(), Some("Makefile:2"));
    }

    #[test]
    fn matches_whole_words_only() {
        let found = scan(
            &[
                ("a.py", b"def load():\n    pass\n"),
                ("b.py", b"reload()\nload_all()\nmy_load = 1\nloader\n"),
            ],
            &[candidate("load", "a.py", 1, 2)],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn punctuation_is_a_word_boundary() {
        for text in [
            &b"x = obj.load()\n"[..],
            b"name: \"load\"\n",
            b"$(load)\n",
            b"load-config\n",
            b"Type::load\n",
        ] {
            let found = scan(
                &[("a.py", b"def load():\n    pass\n"), ("b.txt", text)],
                &[candidate("load", "a.py", 1, 2)],
            );
            assert_eq!(found.len(), 1, "{}", String::from_utf8_lossy(text));
        }
    }

    #[test]
    fn same_named_definitions_block_each_other() {
        // An overload or a duplicate cannot be told from a use.
        let source = b"int area(int a) { return a; }\nint area(int a, int b) { return a * b; }\n";
        let found = scan(
            &[("shapes.cpp", source)],
            &[
                candidate("area", "shapes.cpp", 1, 1),
                candidate("area", "shapes.cpp", 2, 2),
            ],
        );
        assert_eq!(found.len(), 2, "{found:?}");
    }

    #[test]
    fn a_declaration_the_extractor_reported_is_not_a_use() {
        let directory = tempdir().expect("temp dir");
        let header = directory.path().join("w.h");
        let body = directory.path().join("w.cpp");
        fs::write(&header, "void unused_paint(); // unused_paint draws
").expect("write");
        fs::write(&body, "void unused_paint() {
}
").expect("write");
        let files = vec![
            TextFile { path: body, relative: "w.cpp".to_owned() },
            TextFile { path: header, relative: "w.h".to_owned() },
        ];
        let candidates = [candidate("unused_paint", "w.cpp", 1, 2)];
        let mut declarations = HashMap::new();
        declarations.insert("w.h".to_owned(), vec![("unused_paint".to_owned(), 1)]);

        // The prototype is excused; the comment on the same line still counts.
        let found = find_untraced(&files, &candidates, &declarations, 2);
        assert_eq!(found[0].first_mention.as_deref(), Some("w.h:1"));

        fs::write(&files[1].path, "void unused_paint();
").expect("write");
        assert!(find_untraced(&files, &candidates, &declarations, 2).is_empty());
    }

    #[test]
    fn qualified_names_are_looked_up_by_their_last_word() {
        let found = scan(
            &[
                ("w.cpp", b"void Widget::paint() {\n}\n"),
                ("x.cpp", b"w->paint();\n"),
            ],
            &[candidate("Widget::paint", "w.cpp", 1, 2)],
        );
        assert_eq!(found[0].first_mention.as_deref(), Some("x.cpp:1"));
    }

    #[test]
    fn binary_files_are_not_read() {
        let found = scan(
            &[
                ("a.py", b"def load():\n    pass\n"),
                ("blob.bin", b"\x00\x01load\x00"),
            ],
            &[candidate("load", "a.py", 1, 2)],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn prose_is_not_a_use() {
        assert!(is_prose("README.md"));
        assert!(is_prose("docs/guide.rst"));
        assert!(is_prose("CHANGELOG"));
        assert!(is_prose("LICENSE-MIT"));
        assert!(is_prose("crates/x/CHANGELOG.txt"));
        assert!(!is_prose("docs/example.rs"));
        assert!(!is_prose("Makefile"));
        assert!(!is_prose("CMakeLists.txt"));
        assert!(!is_prose("pages/index.mdx"));
        assert!(!is_prose("src/license_check.rs"));
    }
}
