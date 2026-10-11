//! `dbshell`: start the backend's native client (`sqlite3`, `psql`, `mysql`).
//!
//! [`ShellCommand::from_url`] builds the command without running it. Passwords
//! never appear on the command line, where `ps` would show them, and are
//! never printed: `psql` gets `PGPASSWORD` and `mysql` gets `MYSQL_PWD` in its
//! environment. Only the connection parts of the URL are passed on; other
//! query parameters are ignored, except that a PostgreSQL URL is handed to
//! `psql` whole (minus its password, including a `?password=` parameter) so
//! options such as `sslmode` apply.

use crate::connect::{backend_kind, sqlite_path};
use crate::error::CliError;
use siderite_orm::BackendKind;
use std::fmt;
use std::process::Command;
use url::Url;

/// A native client invocation.
#[derive(Clone, PartialEq, Eq)]
pub struct ShellCommand {
    /// Client executable.
    pub program: &'static str,
    /// Arguments. Never contains a password.
    pub args: Vec<String>,
    /// Environment variables to set (name, value); values may be secret.
    envs: Vec<(&'static str, String)>,
}

impl fmt::Debug for ShellCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellCommand")
            .field("program", &self.program)
            .field("args", &self.args)
            .field(
                "env",
                &self.envs.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ShellCommand {
    /// Build the client command for `url`.
    ///
    /// # Errors
    /// A usage error for a URL that names no client (unknown scheme,
    /// in-memory SQLite, malformed URL). Messages never contain the URL.
    pub fn from_url(url: &str) -> Result<Self, CliError> {
        match backend_kind(url) {
            Some(BackendKind::Sqlite) => sqlite(url),
            Some(BackendKind::Postgres) => postgres(url),
            Some(BackendKind::MySql) => mysql(url),
            _ => Err(CliError::usage(
                "dbshell supports sqlite:, postgres:// and mysql:// databases",
            )),
        }
    }

    /// Names of the environment variables the client is started with.
    pub fn env_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.envs.iter().map(|(name, _)| *name)
    }

    /// Run the client attached to the terminal and return its exit code.
    ///
    /// # Errors
    /// [`CliError::Shell`] when the client is not installed or cannot start.
    pub fn run(&self) -> Result<u8, CliError> {
        let status = Command::new(self.program)
            .args(&self.args)
            .envs(self.envs.iter().map(|(name, value)| (name, value)))
            .status()
            .map_err(|err| CliError::Shell {
                program: self.program,
                reason: if err.kind() == std::io::ErrorKind::NotFound {
                    "not found on PATH".to_owned()
                } else {
                    err.kind().to_string()
                },
            })?;
        Ok(status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(1))
    }
}

fn sqlite(url: &str) -> Result<ShellCommand, CliError> {
    let Some(path) = sqlite_path(url) else {
        return Err(CliError::usage(
            "an in-memory SQLite database has no file to open in sqlite3",
        ));
    };
    Ok(ShellCommand {
        program: "sqlite3",
        args: vec![path.to_owned()],
        envs: Vec::new(),
    })
}

fn parse(url: &str) -> Result<Url, CliError> {
    // `url::ParseError` describes the problem without echoing the input.
    Url::parse(url).map_err(|err| CliError::usage(format!("invalid database URL: {err}")))
}

fn postgres(url: &str) -> Result<ShellCommand, CliError> {
    let mut parsed = parse(url)?;
    let mut envs = Vec::new();
    if let Some(password) = parsed.password() {
        envs.push(("PGPASSWORD", percent_decode(password)));
    }
    // A malformed URL cannot carry a password, so this cannot fail on one.
    let _ = parsed.set_password(None);
    // libpq also accepts `?password=`; move it to the environment too.
    let mut query = Vec::new();
    for (name, value) in parsed.query_pairs() {
        match name.as_ref() {
            "password" => envs.push(("PGPASSWORD", value.into_owned())),
            "sslpassword" => {
                return Err(CliError::usage(
                    "dbshell cannot pass `sslpassword` safely; remove it from the URL",
                ));
            }
            _ => query.push((name.into_owned(), value.into_owned())),
        }
    }
    if query.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.query_pairs_mut().clear().extend_pairs(&query);
    }
    Ok(ShellCommand {
        program: "psql",
        args: vec![parsed.to_string()],
        envs,
    })
}

fn mysql(url: &str) -> Result<ShellCommand, CliError> {
    let parsed = parse(url)?;
    let mut args = Vec::new();
    if let Some(host) = parsed.host_str() {
        args.push("--host".to_owned());
        args.push(host.trim_matches(['[', ']']).to_owned());
    }
    if let Some(port) = parsed.port() {
        args.push("--port".to_owned());
        args.push(port.to_string());
    }
    if !parsed.username().is_empty() {
        args.push("--user".to_owned());
        args.push(percent_decode(parsed.username()));
    }
    let database = percent_decode(parsed.path().trim_start_matches('/'));
    if !database.is_empty() {
        args.push("--database".to_owned());
        args.push(database);
    }
    let envs = parsed
        .password()
        .map(|password| vec![("MYSQL_PWD", percent_decode(password))])
        .unwrap_or_default();
    Ok(ShellCommand {
        program: "mysql",
        args,
        envs,
    })
}

/// Decode `%XX` escapes; malformed escapes are kept as written.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && let Some(byte) = std::str::from_utf8(hex)
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn env_value<'a>(cmd: &'a ShellCommand, name: &str) -> Option<&'a str> {
        cmd.envs
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn sqlite_strips_scheme_and_query() {
        for (url, path) in [
            ("sqlite://todo.db?mode=rwc", "todo.db"),
            ("sqlite:todo.db", "todo.db"),
            ("sqlite:///var/data/app.db", "/var/data/app.db"),
        ] {
            let cmd = ShellCommand::from_url(url).unwrap();
            assert_eq!(cmd.program, "sqlite3");
            assert_eq!(cmd.args, [path], "{url}");
            assert_eq!(cmd.env_names().count(), 0);
        }
    }

    #[test]
    fn sqlite_memory_has_nothing_to_open() {
        for url in ["sqlite::memory:", "sqlite://", "sqlite://?mode=memory"] {
            assert!(ShellCommand::from_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn postgres_password_moves_to_the_environment() {
        let cmd =
            ShellCommand::from_url("postgres://ann:p%40ss@db.example:5433/app?sslmode=require")
                .unwrap();
        assert_eq!(cmd.program, "psql");
        assert_eq!(
            cmd.args,
            ["postgres://ann@db.example:5433/app?sslmode=require"]
        );
        assert_eq!(env_value(&cmd, "PGPASSWORD"), Some("p@ss"));
        assert!(
            cmd.args
                .iter()
                .all(|a| !a.contains("p%40ss") && !a.contains("p@ss"))
        );
    }

    #[test]
    fn postgres_query_password_moves_to_the_environment() {
        let cmd = ShellCommand::from_url("postgres://ann@db/app?password=s3cret&sslmode=require")
            .unwrap();
        assert_eq!(cmd.args, ["postgres://ann@db/app?sslmode=require"]);
        assert_eq!(env_value(&cmd, "PGPASSWORD"), Some("s3cret"));
        let only = ShellCommand::from_url("postgres://ann@db/app?password=s3cret").unwrap();
        assert_eq!(only.args, ["postgres://ann@db/app"]);
        assert!(ShellCommand::from_url("postgres://ann@db/app?sslpassword=x").is_err());
    }

    #[test]
    fn postgres_without_password_sets_no_environment() {
        let cmd = ShellCommand::from_url("postgresql://ann@localhost/app").unwrap();
        assert_eq!(cmd.args, ["postgresql://ann@localhost/app"]);
        assert_eq!(cmd.env_names().count(), 0);
    }

    #[test]
    fn mysql_uses_flags_and_environment() {
        let cmd = ShellCommand::from_url("mysql://bo%20b:s3%2Fcret@127.0.0.1:3307/shop").unwrap();
        assert_eq!(cmd.program, "mysql");
        assert_eq!(
            cmd.args,
            [
                "--host",
                "127.0.0.1",
                "--port",
                "3307",
                "--user",
                "bo b",
                "--database",
                "shop"
            ]
        );
        assert_eq!(env_value(&cmd, "MYSQL_PWD"), Some("s3/cret"));
        assert!(cmd.args.iter().all(|a| !a.contains("cret")));
    }

    #[test]
    fn mysql_minimal_url() {
        let cmd = ShellCommand::from_url("mysql://localhost").unwrap();
        assert_eq!(cmd.args, ["--host", "localhost"]);
        assert_eq!(cmd.env_names().count(), 0);
        let v6 = ShellCommand::from_url("mysql://[::1]:3306/db").unwrap();
        assert_eq!(&v6.args[..2], ["--host", "::1"]);
    }

    #[test]
    fn debug_shows_env_names_but_never_values() {
        let cmd = ShellCommand::from_url("postgres://ann:hunter2@h/db").unwrap();
        let shown = format!("{cmd:?}");
        assert!(shown.contains("PGPASSWORD"));
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    #[test]
    fn unsupported_and_malformed_urls_are_usage_errors_without_the_url() {
        let err = ShellCommand::from_url("ftp://u:hunter2@h/x").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(!err.to_string().contains("hunter2"));
        let err = ShellCommand::from_url("postgres://u:hunter2@[bad/x").unwrap_err();
        assert!(!err.to_string().contains("hunter2"), "{err}");
        let err = ShellCommand::from_url("mysql://u:hunter2@h:notaport/x").unwrap_err();
        assert!(!err.to_string().contains("hunter2"), "{err}");
    }

    #[test]
    fn percent_decoding_keeps_malformed_escapes() {
        assert_eq!(percent_decode("a%2Fb%20c"), "a/b c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%C3%A9"), "é");
    }
}
