//! Open a [`Db`] from a database URL.
//!
//! The URL scheme picks the backend: `sqlite:`, `postgres://` (feature
//! `postgres`) or `mysql://` (feature `mysql`). Error messages name the
//! scheme only, never the URL, which may hold a password.

use siderite_backends::sqlite::SqliteBackend;
use siderite_migrations::MigrationError;
use siderite_orm::{BackendKind, Db, OrmError};

/// The scheme of `url` (text before the first `:`), lower-cased, when it is
/// a valid URL scheme.
pub fn url_scheme(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    let valid = first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then(|| scheme.to_ascii_lowercase())
}

/// The backend a URL scheme selects, or `None` for an unknown scheme.
pub fn backend_kind(url: &str) -> Option<BackendKind> {
    match url_scheme(url)?.as_str() {
        "sqlite" => Some(BackendKind::Sqlite),
        "postgres" | "postgresql" => Some(BackendKind::Postgres),
        "mysql" | "mariadb" => Some(BackendKind::MySql),
        "mongodb" | "mongodb+srv" => Some(BackendKind::MongoDb),
        "redis" | "rediss" => Some(BackendKind::Redis),
        _ => None,
    }
}

/// The file of a `sqlite:` URL (`sqlite://app.db?mode=rwc` -> `app.db`),
/// or `None` for an in-memory database.
pub(crate) fn sqlite_path(url: &str) -> Option<&str> {
    let rest = url.split_once(':').map_or("", |(_, rest)| rest);
    let rest = rest.trim_start_matches("//");
    let path = rest.split('?').next().unwrap_or_default();
    (!path.is_empty() && path != ":memory:").then_some(path)
}

/// Open a [`Db`] from a `sqlite:`, `postgres://` or `mysql://` URL.
///
/// PostgreSQL and MySQL URLs need the `postgres` and `mysql` features of this
/// crate.
///
/// # Errors
/// A usage error for an unsupported scheme or a missing feature (the message
/// never contains the URL), or a connection failure.
pub async fn connect_url(url: &str) -> Result<Db, MigrationError> {
    match backend_kind(url) {
        Some(BackendKind::Sqlite) => {
            let backend = SqliteBackend::connect(url).await.map_err(OrmError::from)?;
            Ok(Db::new(backend))
        }
        Some(BackendKind::Postgres) => connect_postgres(url).await,
        Some(BackendKind::MySql) => connect_mysql(url).await,
        _ => Err(MigrationError::usage(match url_scheme(url) {
            Some(scheme) => format!(
                "unsupported database URL scheme `{scheme}` (expected sqlite:, postgres:// or mysql://)"
            ),
            None => {
                "database URL has no scheme (expected sqlite:, postgres:// or mysql://)".to_owned()
            }
        })),
    }
}

#[cfg(feature = "postgres")]
async fn connect_postgres(url: &str) -> Result<Db, MigrationError> {
    let backend = siderite_backends::postgres::PgBackend::connect(url)
        .await
        .map_err(OrmError::from)?;
    Ok(Db::new(backend))
}

#[cfg(not(feature = "postgres"))]
async fn connect_postgres(_url: &str) -> Result<Db, MigrationError> {
    Err(MigrationError::usage(
        "PostgreSQL URLs require siderite-cli built with `--features postgres`",
    ))
}

#[cfg(feature = "mysql")]
async fn connect_mysql(url: &str) -> Result<Db, MigrationError> {
    let backend = siderite_backends::mysql::MySqlBackend::connect(url)
        .await
        .map_err(OrmError::from)?;
    Ok(Db::new(backend))
}

#[cfg(not(feature = "mysql"))]
async fn connect_mysql(_url: &str) -> Result<Db, MigrationError> {
    Err(MigrationError::usage(
        "MySQL URLs require siderite-cli built with `--features mysql`",
    ))
}

/// An in-memory SQLite database, for commands that need a [`Db`] but never
/// touch it (`makemigrations`, `squashmigrations`).
///
/// # Errors
/// A connection failure.
pub async fn scratch_db() -> Result<Db, MigrationError> {
    connect_url("sqlite::memory:").await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn schemes_select_backends() {
        assert_eq!(backend_kind("sqlite::memory:"), Some(BackendKind::Sqlite));
        assert_eq!(backend_kind("SQLITE://a.db"), Some(BackendKind::Sqlite));
        assert_eq!(backend_kind("postgres://h/db"), Some(BackendKind::Postgres));
        assert_eq!(
            backend_kind("postgresql://h/db"),
            Some(BackendKind::Postgres)
        );
        assert_eq!(backend_kind("mysql://h/db"), Some(BackendKind::MySql));
        assert_eq!(backend_kind("mongodb+srv://h"), Some(BackendKind::MongoDb));
        assert_eq!(backend_kind("redis://h"), Some(BackendKind::Redis));
        assert_eq!(backend_kind("ftp://h"), None);
        assert_eq!(backend_kind("no-scheme"), None);
        assert_eq!(backend_kind("1bad://x"), None);
    }

    #[tokio::test]
    async fn sqlite_connects() {
        let db = connect_url("sqlite::memory:").await.unwrap();
        assert_eq!(db.capabilities().kind, BackendKind::Sqlite);
    }

    #[tokio::test]
    async fn unknown_scheme_error_hides_the_url() {
        let err = connect_url("ftp://user:hunter2@host/db").await.unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("unsupported database URL scheme `ftp`"),
            "{text}"
        );
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains("host"), "{text}");
    }

    #[tokio::test]
    async fn missing_scheme_is_a_usage_error() {
        let err = connect_url("just-a-secret").await.unwrap_err();
        let text = err.to_string();
        assert!(text.contains("no scheme"), "{text}");
        assert!(!text.contains("secret"), "{text}");
    }

    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn postgres_urls_need_the_feature() {
        let err = connect_url("postgres://u:hunter2@localhost/db")
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("--features postgres"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[cfg(not(feature = "mysql"))]
    #[tokio::test]
    async fn mysql_urls_need_the_feature() {
        let err = connect_url("mysql://u:hunter2@localhost/db")
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("--features mysql"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
    }

    /// Needs a server: `MYSQL_URL=mysql://... cargo test -p siderite-cli --features mysql -- --ignored`.
    #[cfg(feature = "mysql")]
    #[tokio::test]
    #[ignore = "needs a MySQL server: set MYSQL_URL"]
    async fn mysql_connects() {
        let Some(url) = ["MYSQL_URL", "DATABASE_URL"]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok())
            .find(|url| backend_kind(url) == Some(BackendKind::MySql))
        else {
            eprintln!("MYSQL_URL not set; skipping");
            return;
        };
        let db = connect_url(&url).await.unwrap();
        assert_eq!(db.capabilities().kind, BackendKind::MySql);
    }
}
