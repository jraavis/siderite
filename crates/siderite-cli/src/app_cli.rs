//! [`AppCli`]: the command line of an application binary.

use crate::args::{GlobalArgs, split_global};
use crate::check::{CheckIssue, CheckReport, check, check_build, has_errors};
use crate::connect::{backend_kind, connect_url, scratch_db};
use crate::dbshell::ShellCommand;
use crate::envelope::CliEnvelope;
use crate::error::CliError;
use crate::routes::{RoutesReport, render_routes, route_table};
use crate::settings::{CliSettings, DEFAULT_DATABASE};
use siderite_core::App;
use siderite_orm::router::DatabaseRouter;
use siderite_orm::{BackendKind, Databases, Db, ModelMeta};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

/// The commands that need the application's own [`App`] and models, so they
/// run from the application binary rather than the standalone `siderite` tool.
///
/// | Command | Does |
/// |---|---|
/// | `run [--addr ADDR]` | connects the configured databases and serves the app |
/// | `routes [--json]` | prints `METHOD PATH operation_id` for every documented route |
/// | `check [--json]` | runs [`check`](crate::check()); exits `1` if any error is found |
/// | `dbshell` | starts `sqlite3`, `psql` or `mysql` on the database |
/// | `makemigrations`, `migrate`, `rollback`, `showmigrations`, `squashmigrations` | delegate to [`siderite_migrations::cli::run`] |
///
/// With `--json` (anywhere on the line), `routes` and `check` print one
/// [`CliEnvelope`] on stdout; other commands reject it as a usage error, and
/// failures print an error envelope on stderr. Factories must not write to
/// stdout, or the JSON is no longer one value.
///
/// Global flags: `--database ALIAS` (default `default`), `--database-url URL`
/// and `--migrations-dir DIR`. The listen address is, in order, `--addr`, the
/// `ADDR` environment variable, the configured address, `127.0.0.1:8000`.
/// The database of `migrate`, `rollback`, `showmigrations` and `dbshell` is
/// `--database-url`, then the configured URL of the alias, then (for
/// `default`) the `DATABASE_URL` environment variable.
///
/// ```no_run
/// use siderite_cli::{AppCli, CliSettings};
/// use siderite_core::App;
///
/// #[tokio::main]
/// async fn main() -> std::process::ExitCode {
///     AppCli::new(|| App::new().title("demo"))
///         .settings(CliSettings::new().database("default", "sqlite://demo.db?mode=rwc"))
///         .migrations_dir("migrations")
///         .run()
///         .await
/// }
/// ```
pub struct AppCli {
    factory: Box<dyn Fn() -> App + Send + Sync>,
    models: Vec<&'static ModelMeta>,
    settings: CliSettings,
    migrations_dir: PathBuf,
    configure_db: Box<ConfigureDb>,
    router: Option<Arc<ApplyRouter>>,
}

/// Hook applied to each connected database handle.
type ConfigureDb = dyn Fn(&str, Db) -> Db + Send + Sync;

/// Installs the database router on the registry.
type ApplyRouter = dyn Fn(Databases) -> Databases + Send + Sync;

/// Process environment read once per run, so tests can pass their own.
#[derive(Debug, Clone, Default)]
pub(crate) struct Env {
    pub(crate) addr: Option<String>,
    pub(crate) database_url: Option<String>,
}

impl Env {
    pub(crate) fn from_process() -> Self {
        Self {
            addr: std::env::var("ADDR").ok(),
            database_url: std::env::var("DATABASE_URL").ok(),
        }
    }
}

impl AppCli {
    /// Create the command line for the app that `app` builds.
    ///
    /// `app` is a factory because some commands build the app more than once
    /// (`check` builds a fresh one to test that it can be served).
    pub fn new(app: impl Fn() -> App + Send + Sync + 'static) -> Self {
        Self {
            factory: Box::new(app),
            models: Vec::new(),
            settings: CliSettings::new(),
            migrations_dir: PathBuf::from("migrations"),
            configure_db: Box::new(|_, db| db),
            router: None,
        }
    }

    /// Adjust every database handle `run` connects, before it is
    /// registered with the app: attach [`Signals`](siderite_orm::signals::Signals)
    /// with `db.with_signals(..)`, for example. Receives the alias.
    #[must_use]
    pub fn configure_db(mut self, f: impl Fn(&str, Db) -> Db + Send + Sync + 'static) -> Self {
        self.configure_db = Box::new(f);
        self
    }

    /// Route models to databases with `router` when `run` builds the
    /// app's [`Databases`] registry.
    #[must_use]
    pub fn database_router<R: DatabaseRouter + Clone>(mut self, router: R) -> Self {
        self.router = Some(Arc::new(move |dbs: Databases| {
            dbs.with_router(router.clone())
        }));
        self
    }

    /// Register the compiled model metadata (`User::META`, ...).
    #[must_use]
    pub fn models(mut self, models: &[&'static ModelMeta]) -> Self {
        self.models = models.to_vec();
        self
    }

    /// Set the listen address and database URLs.
    #[must_use]
    pub fn settings(mut self, settings: CliSettings) -> Self {
        self.settings = settings;
        self
    }

    /// Directory of the JSON migrations (default `migrations`).
    #[must_use]
    pub fn migrations_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.migrations_dir = dir.into();
        self
    }

    /// Run the command in `std::env::args`, print any error to stderr and
    /// return the process exit code (`0` success, `1` failure, `2` usage).
    pub async fn run(self) -> ExitCode {
        self.run_from(std::env::args().skip(1)).await
    }

    /// Like [`run`](Self::run) with explicit arguments (no program name).
    pub async fn run_from(self, args: impl IntoIterator<Item = String>) -> ExitCode {
        let args: Vec<String> = args.into_iter().collect();
        match self.execute(&args, &Env::from_process()).await {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                if args.iter().any(|a| a == "--json") {
                    let command = split_global(&args)
                        .ok()
                        .and_then(|(_, rest)| rest.into_iter().find(|a| a != "--json"))
                        .unwrap_or_else(|| "siderite".to_owned());
                    let env: CliEnvelope<()> =
                        CliEnvelope::error(command, error_code(&err), err.to_string());
                    match env.to_json_pretty() {
                        Ok(json) => eprintln!("{json}"),
                        Err(_) => eprintln!("error: {err}"),
                    }
                } else {
                    eprintln!("error: {err}");
                }
                ExitCode::from(err.exit_code())
            }
        }
    }

    pub(crate) async fn execute(&self, args: &[String], env: &Env) -> Result<u8, CliError> {
        let (global, rest) = split_global(args)?;
        // `--json` is global: it may precede the command.
        let rest: Vec<String> = rest.into_iter().filter(|a| a != "--json").collect();
        let Some(command) = rest.first().map(String::as_str) else {
            print_help();
            return Ok(if global.help { 0 } else { 2 });
        };
        if global.help || command == "help" {
            print_help();
            return Ok(0);
        }
        if global.json && !matches!(command, "routes" | "check") {
            return Err(CliError::usage(format!(
                "`{command}` does not support --json (only `routes` and `check` do)"
            )));
        }
        match command {
            "run" | "routes" | "check" | "dbshell" => {
                if rest.iter().any(|a| a == "--help" || a == "-h") {
                    print_help();
                    return Ok(0);
                }
                if let Some(extra) = rest.get(1) {
                    return Err(CliError::usage(format!(
                        "unexpected argument `{extra}` for `{command}`"
                    )));
                }
                match command {
                    "run" => self.serve(&global, env).await,
                    "routes" => {
                        print!("{}", self.routes_output(global.json)?);
                        Ok(0)
                    }
                    "check" => {
                        let (out, code) = self.check_output(global.json)?;
                        print!("{out}");
                        Ok(code)
                    }
                    _ => self.dbshell(&global, env),
                }
            }
            "makemigrations" | "squashmigrations" => {
                // These never touch a database, but the runner wants a handle.
                let db = scratch_db().await?;
                self.migrations(&global, &db, rest).await
            }
            "migrate" | "rollback" | "showmigrations" | "inspectmigrations" => {
                let url = self.database_url(&global, env)?;
                let alias = global.database.as_deref().unwrap_or(DEFAULT_DATABASE);
                let db = connect_url(&url)
                    .await
                    .map_err(|source| CliError::Connect {
                        alias: alias.to_owned(),
                        source,
                    })?;
                self.migrations(&global, &db, rest).await
            }
            other => Err(CliError::usage(format!(
                "unknown command `{other}` (try --help)"
            ))),
        }
    }

    async fn migrations(
        &self,
        global: &GlobalArgs,
        db: &siderite_orm::Db,
        rest: Vec<String>,
    ) -> Result<u8, CliError> {
        let dir = global
            .migrations_dir
            .clone()
            .unwrap_or_else(|| self.migrations_dir.clone());
        let code = siderite_migrations::cli::run(&self.models, db, &dir, rest).await?;
        Ok(code.0)
    }

    /// URL for the database selected by the global flags.
    fn database_url(&self, global: &GlobalArgs, env: &Env) -> Result<String, CliError> {
        if let Some(url) = &global.database_url {
            return Ok(url.clone());
        }
        let alias = global.database.as_deref().unwrap_or(DEFAULT_DATABASE);
        if let Some(url) = self.settings.database_url(alias) {
            return Ok(url.to_owned());
        }
        if alias == DEFAULT_DATABASE
            && let Some(url) = env.database_url.as_deref().filter(|u| !u.is_empty())
        {
            return Ok(url.to_owned());
        }
        Err(CliError::NoDatabase {
            alias: alias.to_owned(),
        })
    }

    /// Build the app with every configured SQL database registered under its
    /// alias. Aliases for other kinds of store (Redis, MongoDB) are left to the
    /// application.
    pub(crate) async fn build_app(&self) -> Result<App, CliError> {
        let app = (self.factory)();
        if self.settings.database_urls().is_empty() && self.router.is_none() {
            return Ok(app);
        }
        let mut databases = app.database_registry().cloned().unwrap_or_default();
        for (alias, url) in self.settings.database_urls() {
            if matches!(
                backend_kind(url),
                Some(BackendKind::Redis | BackendKind::MongoDb)
            ) {
                continue;
            }
            let db = connect_url(url).await.map_err(|source| CliError::Connect {
                alias: alias.clone(),
                source,
            })?;
            databases = databases.with(alias.clone(), (self.configure_db)(alias, db));
        }
        if let Some(router) = &self.router {
            databases = router(databases);
        }
        Ok(app.databases(databases))
    }

    async fn serve(&self, global: &GlobalArgs, env: &Env) -> Result<u8, CliError> {
        let addr = self
            .settings
            .resolve_addr(global.addr.as_deref(), env.addr.as_deref());
        let app = self.build_app().await?;
        println!("Starting server at http://{addr}");
        app.run(&addr).await?;
        Ok(0)
    }

    /// `routes` output: the text table, or one JSON envelope.
    fn routes_output(&self, json: bool) -> Result<String, CliError> {
        let routes = route_table(&(self.factory)())?;
        if !json {
            return Ok(render_routes(&routes));
        }
        to_json(&CliEnvelope::success("routes", RoutesReport { routes }))
    }

    /// `check` output and exit code (`1` when an error was found).
    fn check_output(&self, json: bool) -> Result<(String, u8), CliError> {
        let issues = self.check_issues();
        let code = u8::from(has_errors(&issues));
        if json {
            let mut env = CliEnvelope::success("check", CheckReport::new(issues));
            env.ok = code == 0;
            return Ok((to_json(&env)?, code));
        }
        let mut out = String::new();
        for issue in &issues {
            out.push_str(&format!("{issue}\n"));
        }
        if issues.is_empty() {
            out.push_str("System check identified no issues.\n");
        } else {
            out.push_str(&format!(
                "System check identified {} issue(s).\n",
                issues.len()
            ));
        }
        Ok((out, code))
    }

    fn check_issues(&self) -> Vec<CheckIssue> {
        let mut issues = check(
            &(self.factory)(),
            &self.models,
            &self.settings,
            Some(&self.migrations_dir),
        );
        // A duplicate operation already fails the OpenAPI check; do not
        // report the same route problem a second time.
        if !issues.iter().any(|i| i.id.starts_with("openapi.")) {
            issues.extend(check_build((self.factory)()));
        }
        issues
    }

    fn dbshell(&self, global: &GlobalArgs, env: &Env) -> Result<u8, CliError> {
        let url = self.database_url(global, env)?;
        ShellCommand::from_url(&url)?.run()
    }
}

/// One JSON value plus a trailing newline.
fn to_json<T: serde::Serialize>(env: &CliEnvelope<T>) -> Result<String, CliError> {
    env.to_json_pretty()
        .map(|json| format!("{json}\n"))
        .map_err(|err| CliError::Io(format!("failed to serialize JSON: {err}")))
}

/// Stable diagnostic code for an error envelope.
fn error_code(err: &CliError) -> &'static str {
    match err.exit_code() {
        2 => "USAGE",
        _ => match err {
            CliError::OpenApi(_) => "OPENAPI",
            _ => "ERROR",
        },
    }
}

fn print_help() {
    println!(
        "\
Commands:
  run [--addr ADDR]            Connect the databases and serve the app
  routes [--json]              List METHOD PATH operation_id
  check [--json]               Validate config, models, migrations and routes
  dbshell                      Open the database's native client
  makemigrations [--name SLUG] [--empty] [--dry-run]
  migrate [TARGET] [--dry-run]
  rollback [--steps N | TARGET] [--dry-run]
  showmigrations
  inspectmigrations       Read-only recovery report
  squashmigrations FROM TO [--name SLUG]

Options:
  --addr ADDR             Listen address (default: ADDR, settings, 127.0.0.1:8000)
  --database ALIAS        Database alias for migrate/rollback/showmigrations/dbshell
  --database-url URL      Database URL, overriding the settings
  --migrations-dir DIR    Migration JSON directory
  --json                  One JSON envelope on stdout (routes, check)
  --help                  Show this help
"
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::fixtures::{author, book, no_pk};
    use siderite_core::get;

    fn cli() -> AppCli {
        AppCli::new(|| App::new().route("/ping", get(|| async { "pong" }).operation_id("ping")))
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    async fn run(cli: &AppCli, list: &[&str], env: &Env) -> Result<u8, CliError> {
        cli.execute(&args(list), env).await
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "siderite-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sqlite_url(dir: &std::path::Path) -> String {
        format!("sqlite://{}?mode=rwc", dir.join("app.db").display())
    }

    #[tokio::test]
    async fn no_command_and_help() {
        let env = Env::default();
        assert_eq!(run(&cli(), &[], &env).await.unwrap(), 2);
        assert_eq!(run(&cli(), &["--help"], &env).await.unwrap(), 0);
        assert_eq!(run(&cli(), &["help"], &env).await.unwrap(), 0);
        assert_eq!(run(&cli(), &["routes", "--help"], &env).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn unknown_commands_and_extra_arguments_are_usage_errors() {
        let env = Env::default();
        let err = run(&cli(), &["frobnicate"], &env).await.unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("frobnicate"));
        let err = run(&cli(), &["routes", "extra"], &env).await.unwrap_err();
        assert_eq!(err.exit_code(), 2);
        let err = run(&cli(), &["check", "--nope"], &env).await.unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn routes_and_a_clean_check_succeed() {
        let env = Env::default();
        assert_eq!(run(&cli(), &["routes"], &env).await.unwrap(), 0);
        let dir = temp_dir("check-clean");
        let cli = cli().migrations_dir(dir);
        assert_eq!(run(&cli, &["check"], &env).await.unwrap(), 0);
    }

    fn parse(out: &str) -> serde_json::Value {
        serde_json::from_str(out).unwrap()
    }

    #[tokio::test]
    async fn routes_json_is_one_envelope_without_hidden_routes() {
        let cli = AppCli::new(|| {
            App::new()
                .route("/ping", get(|| async { "pong" }).operation_id("ping"))
                .route("/secret", get(|| async { "s" }).hidden())
        });
        let json = parse(&cli.routes_output(true).unwrap());
        assert_eq!(json["schema_version"], "1.0");
        assert_eq!(json["command"], "routes");
        assert_eq!(json["ok"], true);
        assert_eq!(
            json["data"]["routes"],
            serde_json::json!([
                {"method": "GET", "path": "/ping", "operation_id": "ping"}
            ])
        );
        // Text output is unchanged.
        assert!(cli.routes_output(false).unwrap().starts_with("GET"));
    }

    #[tokio::test]
    async fn json_flag_works_before_or_after_the_command() {
        let env = Env::default();
        let cli = cli().migrations_dir(temp_dir("json-pos"));
        for list in [
            &["--json", "routes"][..],
            &["routes", "--json"],
            &["--json", "check"],
        ] {
            assert_eq!(run(&cli, list, &env).await.unwrap(), 0, "{list:?}");
        }
    }

    #[tokio::test]
    async fn json_is_rejected_by_commands_without_a_json_mode() {
        let env = Env::default();
        for cmd in ["run", "dbshell", "migrate", "makemigrations"] {
            let err = run(&cli(), &[cmd, "--json"], &env).await.unwrap_err();
            assert_eq!(err.exit_code(), 2, "{cmd}");
            assert!(err.to_string().contains("--json"), "{err}");
        }
        let err = run(&cli(), &["routes", "--json", "x"], &env)
            .await
            .unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn routes_json_with_a_broken_spec_is_an_error() {
        let dup = AppCli::new(|| {
            App::new()
                .route("/a", get(|| async { "a" }).operation_id("same"))
                .route("/b", get(|| async { "b" }).operation_id("same"))
        });
        let err = run(&dup, &["routes", "--json"], &Env::default())
            .await
            .unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(error_code(&err), "OPENAPI");
    }

    #[tokio::test]
    async fn check_json_reports_issues_and_fails_on_errors() {
        let dir = temp_dir("check-json");
        let clean = cli().migrations_dir(dir.clone());
        let (out, code) = clean.check_output(true).unwrap();
        let json = parse(&out);
        assert_eq!(code, 0);
        assert_eq!(json["ok"], true);
        assert_eq!(json["data"]["issues"], serde_json::json!([]));

        let broken = cli().models(&[no_pk()]).migrations_dir(dir);
        let (out, code) = broken.check_output(true).unwrap();
        let json = parse(&out);
        assert_eq!(code, 1);
        assert_eq!(json["command"], "check");
        assert_eq!(json["ok"], false);
        assert!(json["data"]["errors"].as_u64().unwrap() >= 1);
        let issues = json["data"]["issues"].as_array().unwrap();
        assert!(
            issues
                .iter()
                .any(|i| i["level"] == "error" && i["id"] == "models.E003"),
            "{issues:?}"
        );
        let (text, _) = broken.check_output(false).unwrap();
        assert!(text.contains("issue(s)."), "{text}");
    }

    #[tokio::test]
    async fn check_fails_on_errors_but_not_on_warnings() {
        let env = Env::default();
        let dir = temp_dir("check-errors");
        let broken = cli().models(&[no_pk()]).migrations_dir(dir.clone());
        assert_eq!(run(&broken, &["check"], &env).await.unwrap(), 1);

        // Registered models and no migrations yet: only a warning, so it passes.
        let warn_only = cli()
            .models(&[author()])
            .settings(CliSettings::new().database("default", "sqlite::memory:"))
            .migrations_dir(dir);
        assert_eq!(run(&warn_only, &["check"], &env).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn check_reports_duplicate_routes() {
        let env = Env::default();
        let dup = AppCli::new(|| {
            App::new()
                .route("/a", get(|| async { "1" }))
                .route("/a", get(|| async { "2" }))
        })
        .migrations_dir(temp_dir("dup"));
        assert_eq!(run(&dup, &["check"], &env).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn migration_commands_round_trip_on_sqlite() {
        let env = Env::default();
        let dir = temp_dir("migrate");
        let url = sqlite_url(&dir);
        let migrations = dir.join("migrations");
        let cli = cli()
            .models(&[author(), book()])
            .settings(CliSettings::new().database("default", url))
            .migrations_dir(migrations.clone());

        assert_eq!(run(&cli, &["makemigrations"], &env).await.unwrap(), 0);
        let files: Vec<_> = std::fs::read_dir(&migrations).unwrap().collect();
        assert_eq!(files.len(), 1);
        assert_eq!(run(&cli, &["migrate"], &env).await.unwrap(), 0);
        assert_eq!(run(&cli, &["showmigrations"], &env).await.unwrap(), 0);
        assert_eq!(run(&cli, &["check"], &env).await.unwrap(), 0);
        assert_eq!(
            run(&cli, &["rollback", "--steps", "1"], &env)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn migrations_dir_flag_overrides_the_builder() {
        let env = Env::default();
        let dir = temp_dir("dirflag");
        let custom = dir.join("custom");
        let cli = cli()
            .models(&[author()])
            .migrations_dir(dir.join("ignored"));
        let flag = format!("--migrations-dir={}", custom.display());
        assert_eq!(
            run(&cli, &["makemigrations", &flag], &env).await.unwrap(),
            0
        );
        assert!(custom.exists());
        assert!(!dir.join("ignored").exists());
    }

    #[tokio::test]
    async fn database_selection_precedence() {
        let cli = cli().settings(
            CliSettings::new()
                .database("default", "sqlite::memory:")
                .database("replica", "sqlite://replica.db"),
        );
        let env = Env {
            database_url: Some("sqlite://from-env.db".into()),
            ..Env::default()
        };
        let none = GlobalArgs::default();
        assert_eq!(cli.database_url(&none, &env).unwrap(), "sqlite::memory:");
        let replica = GlobalArgs {
            database: Some("replica".into()),
            ..GlobalArgs::default()
        };
        assert_eq!(
            cli.database_url(&replica, &env).unwrap(),
            "sqlite://replica.db"
        );
        let flag = GlobalArgs {
            database_url: Some("sqlite://flag.db".into()),
            ..GlobalArgs::default()
        };
        assert_eq!(cli.database_url(&flag, &env).unwrap(), "sqlite://flag.db");

        let bare = self::cli();
        assert_eq!(
            bare.database_url(&none, &env).unwrap(),
            "sqlite://from-env.db"
        );
        // The environment only stands in for the `default` alias.
        let err = bare.database_url(&replica, &env).unwrap_err();
        assert!(matches!(err, CliError::NoDatabase { ref alias } if alias == "replica"));
        let err = bare.database_url(&none, &Env::default()).unwrap_err();
        assert!(matches!(err, CliError::NoDatabase { .. }));
    }

    #[tokio::test]
    async fn migrate_without_a_database_is_an_error() {
        let err = run(&cli(), &["migrate"], &Env::default())
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::NoDatabase { .. }));
    }

    #[tokio::test]
    async fn connect_failures_name_the_alias_not_the_url() {
        let cli = cli().settings(CliSettings::new().database("default", "ftp://u:hunter2@h/db"));
        let err = run(&cli, &["migrate"], &Env::default()).await.unwrap_err();
        let text = err.to_string();
        assert!(text.contains("`default`"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[tokio::test]
    async fn dbshell_rejects_in_memory_sqlite_without_running_a_client() {
        let cli = cli().settings(CliSettings::new().database("default", "sqlite::memory:"));
        let err = run(&cli, &["dbshell"], &Env::default()).await.unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn build_app_registers_sql_databases_and_skips_other_stores() {
        let cli = cli().settings(
            CliSettings::new()
                .database("default", "sqlite::memory:")
                .database("replica", "sqlite::memory:")
                .database("cache", "redis://localhost:6379"),
        );
        let app = cli.build_app().await.unwrap();
        let registry = app.database_registry().unwrap();
        assert!(registry.get("default").is_some());
        assert!(registry.get("replica").is_some());
        assert!(registry.get("cache").is_none());
    }

    #[tokio::test]
    async fn build_app_applies_db_configuration_and_router() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Clone)]
        struct DefaultOnly;
        impl DatabaseRouter for DefaultOnly {
            fn allow_migrate(&self, alias: &str, _: &ModelMeta) -> bool {
                alias == "default"
            }
        }

        let configured = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&configured);
        let cli = cli()
            .settings(
                CliSettings::new()
                    .database("default", "sqlite::memory:")
                    .database("replica", "sqlite::memory:"),
            )
            .configure_db(move |_, db| {
                seen.fetch_add(1, Ordering::SeqCst);
                db
            })
            .database_router(DefaultOnly);
        let app = cli.build_app().await.unwrap();
        let registry = app.database_registry().unwrap();
        assert_eq!(configured.load(Ordering::SeqCst), 2);
        let author = crate::fixtures::author();
        assert!(registry.allow_migrate("default", author));
        assert!(!registry.allow_migrate("replica", author));
    }

    #[tokio::test]
    async fn build_app_without_databases_leaves_the_app_alone() {
        let app = cli().build_app().await.unwrap();
        assert!(app.database_registry().is_none());
    }

    #[tokio::test]
    async fn run_with_a_bad_database_fails_before_binding() {
        let cli = cli().settings(CliSettings::new().database("default", "ftp://h/db"));
        let err = run(&cli, &["run", "--addr", "127.0.0.1:0"], &Env::default())
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::Connect { .. }));
    }
}
