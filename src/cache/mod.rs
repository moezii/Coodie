use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::index::{SourceLanguage, Symbol};

const CURRENT_SCHEMA_VERSION: i64 = 1;
const AST_HITS: &str = "ast_hits";
const AST_MISSES: &str = "ast_misses";
const QUERY_HITS: &str = "query_hits";
const QUERY_MISSES: &str = "query_misses";
const LAST_INDEXED_HEAD: &str = "last_indexed_head";
const LAST_FIND_RESULTS: &str = "last_find_results";

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("failed to create cache directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("sqlite cache error at {path}: {source}")]
    Sqlite {
        path: PathBuf,
        source: rusqlite::Error,
    },
    #[error("failed to serialize cache payload: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("system clock is before the Unix epoch")]
    ClockBeforeEpoch,
}

#[derive(Debug)]
pub struct CacheStore {
    path: PathBuf,
    connection: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheStats {
    pub path: PathBuf,
    pub ast_entries: usize,
    pub query_entries: usize,
    pub embedding_entries: usize,
    pub llm_response_entries: usize,
    pub ast_hits: usize,
    pub ast_misses: usize,
    pub query_hits: usize,
    pub query_misses: usize,
}

impl CacheStore {
    pub fn open_for_root(root: &Path) -> Result<Self, CacheError> {
        Self::open(cache_path_for_root(root))
    }

    pub fn open(path: PathBuf) -> Result<Self, CacheError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| CacheError::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let connection = Connection::open(&path).map_err(|source| CacheError::Sqlite {
            path: path.clone(),
            source,
        })?;
        let store = Self { path, connection };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get_ast_symbols(
        &self,
        content_hash: &str,
        language: &SourceLanguage,
    ) -> Result<Option<Vec<Symbol>>, CacheError> {
        let payload = self
            .connection
            .query_row(
                "SELECT symbols_json FROM ast_cache WHERE content_hash = ?1 AND language = ?2",
                params![content_hash, language.cache_label()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| self.sqlite_error(source))?;

        payload
            .map(|json| serde_json::from_str(&json).map_err(CacheError::Serialize))
            .transpose()
    }

    /// Everything one parse of one file produced, keyed by that file's content.
    ///
    /// The `ast_cache` above stores only the symbol list, which is all `index`
    /// needs. An audit also needs entrypoints, call sites, macro-visible names,
    /// assertion evidence and docstrings — so before this existed, every audit
    /// reparsed every file even when nothing had changed.
    pub fn get_file_facts(
        &self,
        content_hash: &str,
        language: &SourceLanguage,
    ) -> Result<Option<crate::audit::graph::FileFacts>, CacheError> {
        let payload = self.get_file_facts_json(content_hash, language)?;

        // A payload this version cannot read is treated as a miss, not an
        // error: the shape of FileFacts may change between releases and a stale
        // row must never fail an audit.
        Ok(payload.and_then(|json| serde_json::from_str(&json).ok()))
    }

    /// The stored, still-serialised facts for a file. Lets the caller decode
    /// payloads off the thread that owns the connection.
    pub fn get_file_facts_json(
        &self,
        content_hash: &str,
        language: &SourceLanguage,
    ) -> Result<Option<String>, CacheError> {
        self.connection
            .prepare_cached(
                "SELECT facts_json FROM facts_cache WHERE content_hash = ?1 AND language = ?2",
            )
            .and_then(|mut statement| {
                statement
                    .query_row(params![content_hash, language.cache_label()], |row| {
                        row.get::<_, String>(0)
                    })
                    .optional()
            })
            .map_err(|source| self.sqlite_error(source))
    }

    /// Store several already-serialised facts payloads in one transaction.
    ///
    /// One commit per file means one WAL sync per file, which on a cold run of
    /// a large repository costs more than the parsing it caches.
    pub fn put_file_facts_json_batch<'a>(
        &self,
        rows: impl IntoIterator<Item = (&'a str, &'a SourceLanguage, &'a str)>,
    ) -> Result<(), CacheError> {
        let stored_at = now_ms()?;
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|source| self.sqlite_error(source))?;
        {
            let mut statement = transaction
                .prepare_cached(
                    "INSERT OR REPLACE INTO facts_cache                  (content_hash, language, facts_json, stored_at_ms)                  VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(|source| self.sqlite_error(source))?;
            for (content_hash, language, payload) in rows {
                statement
                    .execute(params![content_hash, language.cache_label(), payload, stored_at])
                    .map_err(|source| self.sqlite_error(source))?;
            }
        }
        transaction
            .commit()
            .map_err(|source| self.sqlite_error(source))
    }

    pub fn put_file_facts(
        &self,
        content_hash: &str,
        language: &SourceLanguage,
        facts: &crate::audit::graph::FileFacts,
    ) -> Result<(), CacheError> {
        let payload = serde_json::to_string(facts)?;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO facts_cache                  (content_hash, language, facts_json, stored_at_ms)                  VALUES (?1, ?2, ?3, ?4)",
                params![content_hash, language.cache_label(), payload, now_ms()?],
            )
            .map_err(|source| self.sqlite_error(source))?;
        Ok(())
    }

    pub fn put_ast_symbols(
        &self,
        content_hash: &str,
        language: &SourceLanguage,
        source_bytes: usize,
        symbols: &[Symbol],
    ) -> Result<(), CacheError> {
        let payload = serde_json::to_string(symbols)?;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO ast_cache \
                 (content_hash, language, symbols_json, source_bytes, stored_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    content_hash,
                    language.cache_label(),
                    payload,
                    source_bytes as i64,
                    now_ms()?
                ],
            )
            .map_err(|source| self.sqlite_error(source))?;
        Ok(())
    }

    pub fn get_query_results<T: DeserializeOwned>(
        &self,
        query: &str,
        repo_state_hash: &str,
    ) -> Result<Option<T>, CacheError> {
        let payload = self
            .connection
            .query_row(
                "SELECT results_json FROM query_cache \
                 WHERE query_string = ?1 AND repo_state_hash = ?2",
                params![query, repo_state_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| self.sqlite_error(source))?;

        payload
            .map(|json| serde_json::from_str(&json).map_err(CacheError::Serialize))
            .transpose()
    }

    pub fn put_query_results<T: Serialize>(
        &self,
        query: &str,
        repo_state_hash: &str,
        results: &T,
    ) -> Result<(), CacheError> {
        let payload = serde_json::to_string(results)?;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO query_cache \
                 (query_string, repo_state_hash, results_json, stored_at_ms) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![query, repo_state_hash, payload, now_ms()?],
            )
            .map_err(|source| self.sqlite_error(source))?;
        Ok(())
    }

    pub fn get_llm_response(
        &self,
        prompt_hash: &str,
        model: &str,
    ) -> Result<Option<String>, CacheError> {
        self.connection
            .query_row(
                "SELECT response FROM llm_response_cache WHERE prompt_hash = ?1 AND model = ?2",
                params![prompt_hash, model],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| self.sqlite_error(source))
    }

    pub fn put_llm_response(
        &self,
        prompt_hash: &str,
        model: &str,
        response: &str,
    ) -> Result<(), CacheError> {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO llm_response_cache \
                 (prompt_hash, model, response, stored_at_ms) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![prompt_hash, model, response, now_ms()?],
            )
            .map_err(|source| self.sqlite_error(source))?;
        Ok(())
    }

    pub fn last_find_results<T: DeserializeOwned>(&self) -> Result<Option<T>, CacheError> {
        self.metadata_json(LAST_FIND_RESULTS)
    }

    pub fn set_last_find_results<T: Serialize>(&self, results: &T) -> Result<(), CacheError> {
        self.set_metadata_json(LAST_FIND_RESULTS, results)
    }

    pub fn record_ast_hit(&self) -> Result<(), CacheError> {
        self.increment_counter(AST_HITS)
    }

    pub fn record_ast_miss(&self) -> Result<(), CacheError> {
        self.increment_counter(AST_MISSES)
    }

    pub fn record_query_hit(&self) -> Result<(), CacheError> {
        self.increment_counter(QUERY_HITS)
    }

    pub fn record_query_miss(&self) -> Result<(), CacheError> {
        self.increment_counter(QUERY_MISSES)
    }

    pub fn stats(&self) -> Result<CacheStats, CacheError> {
        Ok(CacheStats {
            path: self.path.clone(),
            ast_entries: self.count_table("ast_cache")?,
            query_entries: self.count_table("query_cache")?,
            embedding_entries: self.count_table("embedding_cache")?,
            llm_response_entries: self.count_table("llm_response_cache")?,
            ast_hits: self.counter(AST_HITS)?,
            ast_misses: self.counter(AST_MISSES)?,
            query_hits: self.counter(QUERY_HITS)?,
            query_misses: self.counter(QUERY_MISSES)?,
        })
    }

    pub fn clear(&self) -> Result<(), CacheError> {
        for table in [
            "ast_cache",
            "query_cache",
            "embedding_cache",
            "llm_response_cache",
            "cache_counters",
            "repo_metadata",
        ] {
            self.connection
                .execute(&format!("DELETE FROM {table}"), [])
                .map_err(|source| self.sqlite_error(source))?;
        }
        Ok(())
    }

    pub fn last_indexed_head(&self) -> Result<Option<String>, CacheError> {
        self.metadata_value(LAST_INDEXED_HEAD)
    }

    pub fn set_last_indexed_head(&self, head_sha: &str) -> Result<(), CacheError> {
        self.set_metadata_value(LAST_INDEXED_HEAD, head_sha)
    }

    fn metadata_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, CacheError> {
        self.metadata_value(key)?
            .map(|json| serde_json::from_str(&json).map_err(CacheError::Serialize))
            .transpose()
    }

    fn set_metadata_json<T: Serialize>(&self, key: &str, value: &T) -> Result<(), CacheError> {
        let payload = serde_json::to_string(value)?;
        self.set_metadata_value(key, &payload)
    }

    fn migrate(&self) -> Result<(), CacheError> {
        self.connection
            .execute_batch(
                "
                PRAGMA journal_mode = WAL;
                PRAGMA foreign_keys = ON;

                CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at_ms INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS ast_cache (
                    content_hash TEXT PRIMARY KEY,
                    language TEXT NOT NULL,
                    symbols_json TEXT NOT NULL,
                    source_bytes INTEGER NOT NULL,
                    stored_at_ms INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS facts_cache (
                    content_hash TEXT NOT NULL,
                    language TEXT NOT NULL,
                    facts_json TEXT NOT NULL,
                    stored_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (content_hash, language)
                );

                CREATE TABLE IF NOT EXISTS query_cache (
                    query_string TEXT NOT NULL,
                    repo_state_hash TEXT NOT NULL,
                    results_json TEXT NOT NULL,
                    stored_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (query_string, repo_state_hash)
                );

                CREATE TABLE IF NOT EXISTS embedding_cache (
                    chunk_hash TEXT PRIMARY KEY,
                    provider TEXT NOT NULL,
                    model TEXT NOT NULL,
                    dimensions INTEGER NOT NULL,
                    vector BLOB NOT NULL,
                    stored_at_ms INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS llm_response_cache (
                    prompt_hash TEXT NOT NULL,
                    model TEXT NOT NULL,
                    response TEXT NOT NULL,
                    stored_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (prompt_hash, model)
                );

                CREATE TABLE IF NOT EXISTS cache_counters (
                    name TEXT PRIMARY KEY,
                    value INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS repo_metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                );
                ",
            )
            .map_err(|source| self.sqlite_error(source))?;

        self.connection
            .execute(
                "INSERT OR IGNORE INTO schema_migrations (version, applied_at_ms) VALUES (?1, ?2)",
                params![CURRENT_SCHEMA_VERSION, now_ms()?],
            )
            .map_err(|source| self.sqlite_error(source))?;

        for counter in [AST_HITS, AST_MISSES, QUERY_HITS, QUERY_MISSES] {
            self.connection
                .execute(
                    "INSERT OR IGNORE INTO cache_counters (name, value) VALUES (?1, 0)",
                    params![counter],
                )
                .map_err(|source| self.sqlite_error(source))?;
        }

        Ok(())
    }

    fn increment_counter(&self, name: &str) -> Result<(), CacheError> {
        self.connection
            .execute(
                "INSERT INTO cache_counters (name, value) VALUES (?1, 1) \
                 ON CONFLICT(name) DO UPDATE SET value = value + 1",
                params![name],
            )
            .map_err(|source| self.sqlite_error(source))?;
        Ok(())
    }

    fn counter(&self, name: &str) -> Result<usize, CacheError> {
        let value = self
            .connection
            .query_row(
                "SELECT value FROM cache_counters WHERE name = ?1",
                params![name],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|source| self.sqlite_error(source))?
            .unwrap_or(0);
        Ok(value as usize)
    }

    fn count_table(&self, table: &str) -> Result<usize, CacheError> {
        let count = self
            .connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|source| self.sqlite_error(source))?;
        Ok(count as usize)
    }

    fn metadata_value(&self, key: &str) -> Result<Option<String>, CacheError> {
        self.connection
            .query_row(
                "SELECT value FROM repo_metadata WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| self.sqlite_error(source))
    }

    fn set_metadata_value(&self, key: &str, value: &str) -> Result<(), CacheError> {
        self.connection
            .execute(
                "INSERT INTO repo_metadata (key, value, updated_at_ms) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at_ms = excluded.updated_at_ms",
                params![key, value, now_ms()?],
            )
            .map_err(|source| self.sqlite_error(source))?;
        Ok(())
    }

    fn sqlite_error(&self, source: rusqlite::Error) -> CacheError {
        CacheError::Sqlite {
            path: self.path.clone(),
            source,
        }
    }
}

pub fn cache_path_for_root(root: &Path) -> PathBuf {
    root.join(".codereader").join("cache.sqlite3")
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    to_lower_hex(&hasher.finalize())
}

pub fn hash_parts(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    to_lower_hex(&hasher.finalize())
}

fn to_lower_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }
    output
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'a' + (value - 10)) as char,
        _ => '0',
    }
}

fn now_ms() -> Result<i64, CacheError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CacheError::ClockBeforeEpoch)?;
    Ok(elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{hash_bytes, CacheStore};

    #[test]
    fn hash_bytes_is_stable_sha256_hex() {
        assert_eq!(
            hash_bytes(b"code-reader"),
            "572505b163c885f8c34282039280b8d979a2277a40d7bdd125c89c3925595637"
        );
    }

    #[test]
    fn cache_stats_and_clear_cover_all_tables() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::open_for_root(directory.path()).unwrap();

        cache.record_ast_hit().unwrap();
        cache.record_ast_miss().unwrap();
        cache.record_query_hit().unwrap();
        cache.record_query_miss().unwrap();
        cache.set_last_indexed_head("abc123").unwrap();
        let stats = cache.stats().unwrap();

        assert_eq!(stats.ast_hits, 1);
        assert_eq!(stats.ast_misses, 1);
        assert_eq!(stats.query_hits, 1);
        assert_eq!(stats.query_misses, 1);
        assert_eq!(
            cache.last_indexed_head().unwrap().as_deref(),
            Some("abc123")
        );

        cache.clear().unwrap();
        let stats = cache.stats().unwrap();

        assert_eq!(stats.ast_entries, 0);
        assert_eq!(stats.query_entries, 0);
        assert_eq!(stats.embedding_entries, 0);
        assert_eq!(stats.llm_response_entries, 0);
        assert_eq!(stats.ast_hits, 0);
        assert_eq!(stats.query_hits, 0);
        assert_eq!(cache.last_indexed_head().unwrap(), None);
    }
}
