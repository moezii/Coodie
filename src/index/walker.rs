use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use super::{IndexError, SourceLanguage};

const DETECTION_PREFIX_BYTES: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: PathBuf,
    pub language: SourceLanguage,
}

pub fn discover_source_files(root: &Path) -> Result<Vec<SourceFile>, IndexError> {
    discover_files(root).map(|walked| walked.source_files)
}

/// Everything one walk of a repository finds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkedFiles {
    /// Files in a recognised language, sorted by path.
    pub source_files: Vec<SourceFile>,
    /// Every other file the walk saw (build files, configs, data, binaries),
    /// sorted by path. Not parsed; read only by checks that need raw text.
    pub other_files: Vec<PathBuf>,
}

/// Walk `root` once, honouring the same ignore rules as
/// [`discover_source_files`], and split what it finds into source files and
/// everything else.
pub fn discover_files(root: &Path) -> Result<WalkedFiles, IndexError> {
    let mut walked = WalkedFiles::default();

    for entry in WalkBuilder::new(root)
        .standard_filters(true)
        .require_git(false)
        .build()
    {
        let entry = entry?;
        let is_file = entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file());
        if !is_file {
            continue;
        }

        match detect_language(entry.path())? {
            Some(language) => walked.source_files.push(SourceFile {
                path: entry.path().to_path_buf(),
                language,
            }),
            None => walked.other_files.push(entry.path().to_path_buf()),
        }
    }

    walked
        .source_files
        .sort_by(|left, right| left.path.cmp(&right.path));
    refine_headers(&mut walked.source_files);
    walked.other_files.sort();
    Ok(walked)
}

/// Decide, per `.h` header, whether it is C or C++.
///
/// A repository whose sources are all C++ (no `.c` file anywhere) has only C++
/// headers. Otherwise each header is read and judged by its own syntax. A
/// header that cannot be read keeps the C reading it already has.
fn refine_headers(files: &mut [SourceFile]) {
    let has_c_source = files
        .iter()
        .any(|file| file.language.id() == "c" && !file.language.is_ambiguous_header(&file.path));
    let has_cpp_source = files.iter().any(|file| file.language.id() == "cpp");
    let cpp_only = has_cpp_source && !has_c_source;

    for file in files.iter_mut() {
        if !file.language.is_ambiguous_header(&file.path) {
            continue;
        }
        let source = if cpp_only {
            Vec::new()
        } else {
            match std::fs::read(&file.path) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            }
        };
        file.language = file
            .language
            .clone()
            .refine_header(&file.path, &source, cpp_only);
    }
}

fn detect_language(path: &Path) -> Result<Option<SourceLanguage>, IndexError> {
    if let Some(language) = SourceLanguage::from_path(path) {
        return Ok(Some(language));
    }

    let mut file = File::open(path).map_err(|source| IndexError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut prefix = vec![0; DETECTION_PREFIX_BYTES];
    let bytes_read = file
        .read(&mut prefix)
        .map_err(|source| IndexError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    prefix.truncate(bytes_read);

    Ok(SourceLanguage::from_path_and_source(path, &prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn names(files: &[SourceFile], root: &Path) -> Vec<String> {
        files
            .iter()
            .map(|file| {
                file.path
                    .strip_prefix(root)
                    .unwrap_or(&file.path)
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/")
            })
            .collect()
    }

    #[test]
    fn finds_source_files_and_skips_everything_else() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        fs::write(root.join("app.py"), "def f():\n    pass\n").expect("write");
        fs::write(root.join("index.ts"), "export const x = 1;\n").expect("write");
        fs::write(root.join("photo.png"), [0u8, 1, 2]).expect("write");

        let files = discover_source_files(root).expect("walk");
        let found = names(&files, root);

        assert!(found.contains(&"app.py".to_owned()), "{found:?}");
        assert!(found.contains(&"index.ts".to_owned()), "{found:?}");
        assert!(!found.contains(&"photo.png".to_owned()), "{found:?}");
    }

    #[test]
    fn one_walk_also_returns_the_files_it_did_not_parse() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        fs::write(root.join(".gitignore"), "ignored.cfg
").expect("write");
        fs::write(root.join("app.py"), "def f():
    pass
").expect("write");
        fs::write(root.join("setup.cfg"), "[x]
").expect("write");
        fs::write(root.join("ignored.cfg"), "[x]
").expect("write");

        let walked = discover_files(root).expect("walk");
        let sources = names(&walked.source_files, root);
        let others: Vec<String> = walked
            .other_files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(sources, vec!["app.py".to_owned()]);
        assert!(others.contains(&"setup.cfg".to_owned()), "{others:?}");
        assert!(!others.contains(&"ignored.cfg".to_owned()), "{others:?}");
    }

    #[test]
    fn respects_gitignore() {
        // Auditing a repository must not walk into whatever the project has
        // already declared is not its source.
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        fs::write(root.join(".gitignore"), "secret.py\nbuild/\n").expect("write");
        fs::write(root.join("kept.py"), "def kept():\n    pass\n").expect("write");
        fs::write(root.join("secret.py"), "def hidden():\n    pass\n").expect("write");
        fs::create_dir_all(root.join("build")).expect("dirs");
        fs::write(root.join("build/out.py"), "def generated():\n    pass\n").expect("write");

        let files = discover_source_files(root).expect("walk");
        let found = names(&files, root);

        assert!(found.contains(&"kept.py".to_owned()), "{found:?}");
        assert!(!found.iter().any(|name| name.contains("secret")), "{found:?}");
        assert!(!found.iter().any(|name| name.starts_with("build/")), "{found:?}");
    }

    #[test]
    fn results_are_sorted_so_two_runs_agree() {
        // Ordering feeds symbol ids and therefore the whole report; an unstable
        // walk would make two audits of unchanged code disagree.
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        for name in ["c.py", "a.py", "b.py"] {
            fs::write(root.join(name), "def f():\n    pass\n").expect("write");
        }

        let first = names(&discover_source_files(root).expect("walk"), root);
        let second = names(&discover_source_files(root).expect("walk"), root);

        assert_eq!(first, second);
        let mut sorted = first.clone();
        sorted.sort();
        assert_eq!(first, sorted);
    }

    fn language_of(files: &[SourceFile], root: &Path, name: &str) -> String {
        files
            .iter()
            .find(|file| file.path.strip_prefix(root).ok() == Some(Path::new(name)))
            .map(|file| file.language.id().to_owned())
            .unwrap_or_default()
    }

    #[test]
    fn headers_in_an_all_cpp_repository_are_cpp() {
        // Neither chess engine benchmarked has a single `.c` file; reading their
        // headers as C lost every class and method declared in them.
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        fs::write(root.join("engine.cpp"), "int main() { return 0; }
").expect("write");
        fs::write(root.join("plain.h"), "int helper(int x);
").expect("write");

        let files = discover_source_files(root).expect("walk");
        assert_eq!(language_of(&files, root, "plain.h"), "cpp");
        assert_eq!(language_of(&files, root, "engine.cpp"), "cpp");
    }

    #[test]
    fn headers_in_a_mixed_repository_are_judged_by_their_own_syntax() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        fs::write(root.join("main.c"), "int main(void) { return 0; }
").expect("write");
        fs::write(root.join("app.cpp"), "int run() { return 0; }
").expect("write");
        fs::write(
            root.join("c_api.h"),
            "#ifdef __cplusplus
extern \"C\" {
#endif
// std::vector in a comment
int helper(int x);
#ifdef __cplusplus
}
#endif
",
        )
        .expect("write");
        fs::write(
            root.join("engine.h"),
            "namespace chess {
class Engine {
 public:
  void go();
};
}
",
        )
        .expect("write");

        let files = discover_source_files(root).expect("walk");
        assert_eq!(language_of(&files, root, "c_api.h"), "c");
        assert_eq!(language_of(&files, root, "engine.h"), "cpp");
        assert_eq!(language_of(&files, root, "main.c"), "c");
    }

    #[test]
    fn an_empty_directory_yields_nothing_rather_than_failing() {
        let directory = tempdir().expect("temp dir");
        assert!(discover_source_files(directory.path())
            .expect("walk")
            .is_empty());
    }
}
