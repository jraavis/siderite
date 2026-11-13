//! Where and how to open connections: the database URL, open flags and the
//! per-connection setup.
//!
//! URLs go through SQLx's own parser first, so the native backend accepts
//! and rejects exactly what [`SqliteBackend::connect`](crate::sqlite::SqliteBackend::connect)
//! does, with the same error messages.

use super::NativeSqliteOptions;
use super::exec;
use rusqlite::{Connection, OpenFlags};
use siderite_orm::BackendError;
use sqlx::sqlite::SqliteConnectOptions;
use std::time::Duration;

/// A parsed database location plus the setup every connection gets.
#[derive(Debug)]
pub(super) struct Target {
    /// File name, or a `file:` URI when `immutable` or `vfs` was given.
    path: String,
    flags: OpenFlags,
    memory: bool,
    /// `PRAGMA` statements run on every new connection.
    setup: String,
    busy_timeout: Duration,
    statement_cache: usize,
}

impl Target {
    /// Parse `url` the way the SQLx backend does and combine it with the
    /// connection options.
    pub(super) fn parse(url: &str, options: &NativeSqliteOptions) -> Result<Self, BackendError> {
        let parsed: SqliteConnectOptions = url
            .parse()
            .map_err(|e: sqlx::Error| BackendError::Connection(e.to_string()))?;
        let (database, query) = split(url);
        let mut memory = database == ":memory:";
        let (mut read_only, mut create, mut shared) = (false, false, memory);
        let (mut immutable, mut vfs) = (false, None);
        // SQLx has already rejected unknown keys and values.
        for (key, value) in query_pairs(query) {
            match (key.as_str(), value.as_str()) {
                ("mode", "ro") => read_only = true,
                ("mode", "rwc") => create = true,
                ("mode", "memory") => (memory, shared) = (true, true),
                ("cache", cache) => shared = cache == "shared",
                ("immutable", flag) => immutable = matches!(flag, "true" | "1"),
                ("vfs", _) => vfs = Some(value),
                _ => {}
            }
        }
        let mut flags = OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        flags |= if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else if create || memory {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        // One connection holds an in-memory database, so it never shares a
        // cache with anything.
        flags |= if shared && !memory {
            OpenFlags::SQLITE_OPEN_SHARED_CACHE
        } else {
            OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
        };
        let path = if memory {
            ":memory:".to_owned()
        } else {
            let file = parsed.get_filename().to_str().ok_or_else(|| {
                BackendError::Connection("filename passed to SQLite must be valid UTF-8".into())
            })?;
            file_uri(file, immutable, vfs.as_deref())
        };
        Ok(Self {
            path,
            flags,
            memory,
            setup: setup(options),
            busy_timeout: options.busy_timeout,
            statement_cache: options.statement_cache,
        })
    }

    /// Whether this is an in-memory database (one connection holds it).
    pub(super) fn in_memory(&self) -> bool {
        self.memory
    }

    /// Open and configure one connection. Blocking: file I/O and pragmas.
    pub(super) fn open(&self) -> rusqlite::Result<Connection> {
        let conn = if self.memory {
            Connection::open_in_memory_with_flags(self.flags)?
        } else {
            Connection::open_with_flags(&self.path, self.flags)?
        };
        conn.busy_timeout(self.busy_timeout)?;
        conn.set_prepared_statement_cache_capacity(self.statement_cache);
        exec::run_script(&conn, &self.setup)?;
        Ok(conn)
    }
}

/// The pragmas for every connection, in the order SQLx applies them.
fn setup(options: &NativeSqliteOptions) -> String {
    let mut sql = String::new();
    if let Some(mode) = options.journal_mode {
        sql.push_str("PRAGMA journal_mode = ");
        sql.push_str(mode.as_sql());
        sql.push_str("; ");
    }
    sql.push_str(if options.foreign_keys {
        "PRAGMA foreign_keys = ON; "
    } else {
        "PRAGMA foreign_keys = OFF; "
    });
    if let Some(level) = options.synchronous {
        sql.push_str("PRAGMA synchronous = ");
        sql.push_str(level.as_sql());
        sql.push_str("; ");
    }
    sql
}

/// The database part and the query string of a SQLite URL, as SQLx splits
/// them.
fn split(url: &str) -> (&str, Option<&str>) {
    let rest = url
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:");
    match rest.split_once('?') {
        Some((database, query)) => (database, Some(query)),
        None => (rest, None),
    }
}

/// `application/x-www-form-urlencoded` pairs, decoded.
fn query_pairs(query: Option<&str>) -> impl Iterator<Item = (String, String)> {
    query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key), decode(value))
        })
}

/// Percent-decode `text`, with `+` as a space. Invalid escapes stay as they
/// are; invalid UTF-8 is replaced.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        let escaped = bytes
            .get(i + 1..i + 3)
            .filter(|_| byte == b'%')
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (escaped, byte) {
            (Some(decoded), _) => {
                out.push(decoded);
                i += 3;
            }
            (None, b'+') => {
                out.push(b' ');
                i += 1;
            }
            (None, other) => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `file` as SQLite opens it: as is, or as a `file:` URI carrying
/// `immutable` and `vfs`, encoded the way SQLx encodes them.
fn file_uri(file: &str, immutable: bool, vfs: Option<&str>) -> String {
    if !immutable && vfs.is_none() {
        return file.to_owned();
    }
    let mut uri = String::from("file:");
    encode(&mut uri, file);
    let mut separator = '?';
    if immutable {
        uri.push_str("?immutable=true");
        separator = '&';
    }
    if let Some(vfs) = vfs {
        uri.push(separator);
        uri.push_str("vfs=");
        encode(&mut uri, vfs);
    }
    uri
}

/// Append `text` with every byte but ASCII letters and digits escaped.
fn encode(out: &mut String, text: &str) {
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() {
            out.push(char::from(byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0xF)]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(url: &str) -> Result<Target, BackendError> {
        Target::parse(url, &NativeSqliteOptions::default())
    }

    #[test]
    fn accepts_the_sqlx_url_forms() -> Result<(), BackendError> {
        for url in [
            "sqlite::memory:",
            "sqlite://:memory:",
            "sqlite://?mode=memory",
        ] {
            assert!(target(url)?.in_memory(), "{url}");
        }
        let file = target("sqlite://data/app.db?mode=rwc")?;
        assert!(!file.in_memory());
        assert_eq!(file.path, "data/app.db");
        assert!(file.flags.contains(OpenFlags::SQLITE_OPEN_CREATE));
        let read_only = target("sqlite:app.db?mode=ro&cache=shared")?;
        assert!(read_only.flags.contains(OpenFlags::SQLITE_OPEN_READ_ONLY));
        assert!(
            read_only
                .flags
                .contains(OpenFlags::SQLITE_OPEN_SHARED_CACHE)
        );
        let spaced = target("sqlite://my%20app.db")?;
        assert_eq!(spaced.path, "my app.db");
        let uri = target("sqlite://a b.db?immutable=true&vfs=unix-none")?;
        assert_eq!(uri.path, "file:a%20b%2Edb?immutable=true&vfs=unix%2Dnone");
        Ok(())
    }

    #[test]
    fn rejects_what_sqlx_rejects() {
        for url in ["sqlite://app.db?mode=bogus", "sqlite://app.db?colour=red"] {
            let Err(BackendError::Connection(message)) = target(url) else {
                panic!("{url} was accepted");
            };
            let sqlx = url
                .parse::<SqliteConnectOptions>()
                .err()
                .map(|e| e.to_string());
            assert_eq!(Some(message), sqlx);
        }
    }

    #[test]
    fn setup_follows_the_options() {
        let options = NativeSqliteOptions {
            journal_mode: Some(super::super::JournalMode::Wal),
            synchronous: Some(super::super::Synchronous::Normal),
            foreign_keys: false,
            ..NativeSqliteOptions::default()
        };
        assert_eq!(
            setup(&options),
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = OFF; PRAGMA synchronous = NORMAL; "
        );
        assert_eq!(
            setup(&NativeSqliteOptions::default()),
            "PRAGMA foreign_keys = ON; "
        );
    }
}
