use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::IndexError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRepository {
    root: PathBuf,
    executable: PathBuf,
}

/// Last-edit provenance for a span of code, surfaced so a reviewer knows what to scrutinize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub author: String,
    pub date: String,
    pub short_sha: String,
    pub summary: String,
}

impl GitRepository {
    pub fn discover(path: &Path) -> Result<Option<Self>, IndexError> {
        let Some(executable) = discover_git_executable() else {
            return Ok(None);
        };

        let output = Command::new(&executable)
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|source| IndexError::GitIo {
                executable: executable.clone(),
                source,
            })?;

        if !output.status.success() {
            return Ok(None);
        }

        let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if root.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self {
            root: PathBuf::from(root),
            executable,
        }))
    }

    pub fn head_sha(&self) -> Result<Option<String>, IndexError> {
        let output = self.run(["rev-parse", "HEAD"])?;
        if !output.status.success() {
            return Ok(None);
        }

        let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if sha.is_empty() {
            Ok(None)
        } else {
            Ok(Some(sha))
        }
    }

    pub fn diff_paths(&self, from: &str, to: &str) -> Result<Vec<PathBuf>, IndexError> {
        let output = self.run(["diff", "--name-only", "--relative", from, to, "--"])?;
        self.require_success(
            &output,
            &["diff", "--name-only", "--relative", from, to, "--"],
        )?;
        Ok(paths_from_lines(&String::from_utf8_lossy(&output.stdout)))
    }

    pub fn dirty_paths(&self) -> Result<Vec<PathBuf>, IndexError> {
        let output = self.run(["status", "--porcelain=v1", "--untracked-files=all"])?;
        self.require_success(
            &output,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
        Ok(parse_status_paths(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Most recent commit that touched the given line range, via `git blame`.
    /// Returns `None` for untracked or uncommitted spans (blame has nothing to report).
    pub fn last_edit(
        &self,
        file: &Path,
        start_line: usize,
        end_line: usize,
    ) -> Result<Option<Provenance>, IndexError> {
        let range = format!("{start_line},{end_line}");
        let file = file.to_string_lossy().to_string();
        let blame = self.run([
            OsString::from("blame"),
            OsString::from("-L"),
            OsString::from(range),
            OsString::from("--porcelain"),
            OsString::from("--"),
            OsString::from(&file),
        ])?;
        if !blame.status.success() {
            return Ok(None);
        }

        let Some(sha) = newest_sha_from_porcelain(&String::from_utf8_lossy(&blame.stdout)) else {
            return Ok(None);
        };

        let show = self.run([
            OsString::from("show"),
            OsString::from("-s"),
            OsString::from("--date=short"),
            OsString::from("--format=%an%x1f%ad%x1f%h%x1f%s"),
            OsString::from(&sha),
        ])?;
        if !show.status.success() {
            return Ok(None);
        }

        Ok(parse_show_provenance(&String::from_utf8_lossy(
            &show.stdout,
        )))
    }

    fn run<I, S>(&self, args: I) -> Result<Output, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Command::new(&self.executable)
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .map_err(|source| IndexError::GitIo {
                executable: self.executable.clone(),
                source,
            })
    }

    fn require_success(&self, output: &Output, args: &[&str]) -> Result<(), IndexError> {
        if output.status.success() {
            return Ok(());
        }

        Err(IndexError::GitFailed {
            executable: self.executable.clone(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

pub fn discover_git_executable() -> Option<PathBuf> {
    if let Some(candidate) = env::var_os("CODEREADER_GIT").map(PathBuf::from) {
        if command_succeeds(&candidate) {
            return Some(candidate);
        }
    }

    find_executable_on_path("git").filter(|candidate| command_succeeds(candidate))
}

fn find_executable_on_path(command_name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let candidates = executable_names(command_name);

    for directory in env::split_paths(&path) {
        for candidate in &candidates {
            let path = directory.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }

    None
}

fn executable_names(command_name: &str) -> Vec<OsString> {
    if Path::new(command_name).extension().is_some() {
        return vec![OsString::from(command_name)];
    }

    let mut names = vec![OsString::from(command_name)];

    #[cfg(windows)]
    {
        let extensions = env::var_os("PATHEXT")
            .and_then(|value| value.into_string().ok())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned());
        for extension in extensions
            .split(';')
            .filter(|extension| !extension.is_empty())
        {
            names.push(OsString::from(format!("{command_name}{extension}")));
            names.push(OsString::from(format!(
                "{command_name}{}",
                extension.to_ascii_lowercase()
            )));
        }
    }

    names
}

fn command_succeeds(executable: &Path) -> bool {
    if executable.components().count() > 1 && !executable.is_file() {
        return false;
    }

    Command::new(executable)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn paths_from_lines(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(clean_git_path)
        .filter(|path| !is_internal_cache_path(path))
        .collect()
}

fn parse_status_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| line.get(3..))
        .map(|path| path.split(" -> ").last().unwrap_or(path).trim())
        .filter(|path| !path.is_empty())
        .map(clean_git_path)
        .filter(|path| !is_internal_cache_path(path))
        .collect()
}

/// Pick the sha of the most recent commit (max author-time) in blame porcelain output.
fn newest_sha_from_porcelain(text: &str) -> Option<String> {
    let mut current_sha: Option<String> = None;
    let mut best: Option<(String, i64)> = None;

    for line in text.lines() {
        if let Some(sha) = porcelain_header_sha(line) {
            current_sha = Some(sha);
            continue;
        }
        if let Some(rest) = line.strip_prefix("author-time ") {
            if let (Some(sha), Ok(time)) = (current_sha.as_ref(), rest.trim().parse::<i64>()) {
                if best
                    .as_ref()
                    .is_none_or(|(_, best_time)| time >= *best_time)
                {
                    best = Some((sha.clone(), time));
                }
            }
        }
    }

    best.map(|(sha, _)| sha)
}

fn porcelain_header_sha(line: &str) -> Option<String> {
    let mut parts = line.split(' ');
    let sha = parts.next()?;
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    // A header is `<sha> <orig-line> <final-line>[ <num-lines>]`.
    if parts.clone().count() < 2 || !parts.all(|part| part.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    Some(sha.to_owned())
}

fn parse_show_provenance(text: &str) -> Option<Provenance> {
    let line = text.lines().next()?;
    let mut fields = line.split('\u{1f}');
    let author = fields.next()?.trim().to_owned();
    let date = fields.next()?.trim().to_owned();
    let short_sha = fields.next()?.trim().to_owned();
    let summary = fields.next().unwrap_or("").trim().to_owned();
    if author.is_empty() || short_sha.is_empty() {
        return None;
    }
    Some(Provenance {
        author,
        date,
        short_sha,
        summary,
    })
}

fn clean_git_path(path: &str) -> PathBuf {
    let path = path.trim().trim_matches('"').replace("\\\"", "\"");
    PathBuf::from(path)
}

fn is_internal_cache_path(path: &Path) -> bool {
    path.components()
        .next()
        .map(|component| component.as_os_str() == ".codereader")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{newest_sha_from_porcelain, parse_show_provenance, parse_status_paths};

    #[test]
    fn newest_sha_picks_most_recent_blame_commit() {
        let porcelain = "\
1111111111111111111111111111111111111111 1 1 1
author Alice
author-time 1000
summary first
\tdef a():
2222222222222222222222222222222222222222 2 2 1
author Bob
author-time 2000
summary second
\t    return 1
";
        assert_eq!(
            newest_sha_from_porcelain(porcelain).as_deref(),
            Some("2222222222222222222222222222222222222222")
        );
    }

    #[test]
    fn parses_show_provenance_fields() {
        let provenance = parse_show_provenance("Alice\u{1f}2025-06-01\u{1f}ab12cd3\u{1f}fix login")
            .expect("provenance parsed");
        assert_eq!(provenance.author, "Alice");
        assert_eq!(provenance.date, "2025-06-01");
        assert_eq!(provenance.short_sha, "ab12cd3");
        assert_eq!(provenance.summary, "fix login");
    }

    #[test]
    fn parses_dirty_status_paths() {
        let paths = parse_status_paths(
            " M src/app.py\n?? new.py\nR  old.py -> renamed.py\n?? .codereader/cache.sqlite3\n",
        );

        assert_eq!(paths.len(), 3);
        assert_eq!(paths[0], PathBuf::from("src/app.py"));
        assert_eq!(paths[1], PathBuf::from("new.py"));
        assert_eq!(paths[2], PathBuf::from("renamed.py"));
    }
}
