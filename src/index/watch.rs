use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};

use super::{reindex_changed_file, IndexError};

pub fn watch_path<W: Write>(path: impl AsRef<Path>, writer: &mut W) -> Result<(), IndexError> {
    let root = path
        .as_ref()
        .canonicalize()
        .map_err(|source| IndexError::CanonicalizePath {
            path: path.as_ref().to_path_buf(),
            source,
        })?;

    let initial = super::index_path(&root)?;
    writeln!(
        writer,
        "Watching {} ({} source file(s), {} function(s))",
        initial.root().display(),
        initial.files_parsed(),
        initial.functions_found()
    )
    .map_err(IndexError::WatchOutput)?;

    let (sender, receiver) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |result| {
            let _ = sender.send(result);
        },
        Config::default(),
    )
    .map_err(IndexError::Notify)?;

    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(IndexError::Notify)?;

    loop {
        let event = receiver
            .recv()
            .map_err(|_| IndexError::WatchChannelClosed)?;
        let event = event.map_err(IndexError::Notify)?;
        for path in source_paths_from_event(event) {
            if let Some(update) = reindex_changed_file(&root, &path)? {
                writeln!(
                    writer,
                    "Reindexed {}: functions={}, cache_hit={}, reparsed={}",
                    update.path.display(),
                    update.functions_found,
                    update.cache_hit,
                    update.reparsed
                )
                .map_err(IndexError::WatchOutput)?;
                writer.flush().map_err(IndexError::WatchOutput)?;
            }
        }
    }
}

fn source_paths_from_event(event: Event) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();

    for path in event.paths {
        if path
            .components()
            .any(|component| component.as_os_str() == ".codereader")
        {
            continue;
        }
        paths.insert(path);
    }

    paths.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{EventKind, ModifyKind};

    fn modify_event(paths: Vec<PathBuf>) -> Event {
        Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths,
            attrs: Default::default(),
        }
    }

    #[test]
    fn passes_through_edits_to_source_files() {
        let event = modify_event(vec![PathBuf::from("src/app.py")]);
        assert_eq!(
            source_paths_from_event(event),
            vec![PathBuf::from("src/app.py")]
        );
    }

    #[test]
    fn ignores_our_own_cache_writes() {
        // The cache lives inside the watched tree. Without this, indexing writes
        // to it, the watcher fires, and the watcher indexes again forever.
        let event = modify_event(vec![PathBuf::from(".codereader/cache.sqlite3")]);
        assert!(source_paths_from_event(event).is_empty());

        let nested = modify_event(vec![PathBuf::from("repo/.codereader/cache.sqlite3-wal")]);
        assert!(source_paths_from_event(nested).is_empty());
    }

    #[test]
    fn keeps_real_edits_alongside_cache_noise() {
        let event = modify_event(vec![
            PathBuf::from(".codereader/cache.sqlite3"),
            PathBuf::from("src/app.py"),
        ]);
        assert_eq!(
            source_paths_from_event(event),
            vec![PathBuf::from("src/app.py")]
        );
    }

    #[test]
    fn duplicate_paths_in_one_event_are_reported_once() {
        let event = modify_event(vec![
            PathBuf::from("src/app.py"),
            PathBuf::from("src/app.py"),
        ]);
        assert_eq!(source_paths_from_event(event).len(), 1);
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    /// A writer the watcher thread can fill while the test reads it.
    #[derive(Clone)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("lock")).into_owned()
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Poll until the watcher has written `needle`, or give up.
    fn wait_for(buffer: &SharedBuffer, needle: &str, limit: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < limit {
            if buffer.text().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn watching_reports_startup_and_reindexes_a_live_edit() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path().to_path_buf();
        std::fs::write(root.join("app.py"), "def one():\n    return 1\n").expect("write");

        let buffer = SharedBuffer::new();
        let watch_root = root.clone();
        let mut watch_buffer = buffer.clone();
        // watch_path never returns, so it runs detached; the process exits with
        // the test binary.
        std::thread::spawn(move || {
            let _ = watch_path(watch_root, &mut watch_buffer);
        });

        assert!(
            wait_for(&buffer, "Watching", Duration::from_secs(15)),
            "watcher never reported startup: {}",
            buffer.text()
        );
        assert!(
            buffer.text().contains("1 source file(s)"),
            "startup line should count what it indexed: {}",
            buffer.text()
        );

        // An edit after the watch is established must be picked up.
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(
            root.join("app.py"),
            "def one():\n    return 1\n\ndef two():\n    return 2\n",
        )
        .expect("write");

        assert!(
            wait_for(&buffer, "Reindexed", Duration::from_secs(20)),
            "an edit did not trigger a reindex: {}",
            buffer.text()
        );
        assert!(
            buffer.text().contains("app.py"),
            "the reindex should name the file that changed: {}",
            buffer.text()
        );
    }

    #[test]
    fn watching_a_missing_directory_fails_rather_than_hanging() {
        let mut buffer = SharedBuffer::new();
        let error = watch_path("does-not-exist-anywhere", &mut buffer).unwrap_err();
        assert!(
            matches!(error, IndexError::CanonicalizePath { .. }),
            "{error:?}"
        );
    }
}
