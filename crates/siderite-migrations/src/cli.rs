//! Reusable command runner (`makemigrations`, `migrate`, `rollback`,
//! `showmigrations`, `squashmigrations`).
//!
//! Applications call [`run`] from their own binary so `makemigrations` can
//! see compiled [`siderite_orm::ModelMeta`]. The `siderite` CLI binary only
//! applies JSON files; see `docs/MIGRATIONS.md`.

use crate::autodetector;
use crate::error::MigrationError;
use crate::executor::{Migrator, Report};
use crate::loader::{self, MigrationGraph};
use crate::migration::{Migration, next_id, sanitize_slug, slug_from_operations};
use crate::squash;
use crate::state::ProjectState;
use siderite_orm::{Db, ModelMeta};
use std::path::Path;

/// Process exit status (`0` is success).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitCode(pub u8);

impl ExitCode {
    /// Success.
    pub const SUCCESS: Self = Self(0);
    /// Command failed.
    pub const FAILURE: Self = Self(1);
    /// Usage / argument error.
    pub const USAGE: Self = Self(2);
}

/// Run a migrations command.
///
/// `models` is the compiled metadata (`User::META`, …). `dir` is the
/// migrations directory. `args` is the command plus flags, without the
/// program name (`["makemigrations", "--name", "init"]`).
///
/// # Errors
/// Graph, IO, JSON, ORM, or usage errors.
pub async fn run(
    models: &[&'static ModelMeta],
    db: &Db,
    dir: &Path,
    args: impl IntoIterator<Item = String>,
) -> Result<ExitCode, MigrationError> {
    let argv: Vec<String> = args.into_iter().collect();
    let parsed = parse_args(&argv)?;
    if parsed.help {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }
    let command = parsed
        .command
        .as_deref()
        .ok_or_else(|| MigrationError::usage("missing command (try --help)"))?;
    match command {
        "makemigrations" => cmd_makemigrations(models, dir, &parsed),
        "migrate" => cmd_migrate(db, dir, &parsed).await,
        "rollback" => cmd_rollback(db, dir, &parsed).await,
        "showmigrations" => cmd_show(db, dir).await,
        "inspectmigrations" => cmd_inspect(db, dir).await,
        "squashmigrations" => cmd_squash(dir, &parsed),
        other => Err(MigrationError::usage(format!(
            "unknown command `{other}` (try --help)"
        ))),
    }
}

struct Parsed {
    command: Option<String>,
    positional: Vec<String>,
    dry_run: bool,
    empty: bool,
    name: Option<String>,
    steps: Option<u32>,
    help: bool,
    lock_timeout_ms: u64,
}

fn parse_args(argv: &[String]) -> Result<Parsed, MigrationError> {
    let mut parsed = Parsed {
        command: None,
        positional: Vec::new(),
        dry_run: false,
        empty: false,
        name: None,
        steps: None,
        help: false,
        lock_timeout_ms: 30_000,
    };
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--help" || a == "-h" {
            parsed.help = true;
            i += 1;
            continue;
        }
        if a == "--dry-run" {
            parsed.dry_run = true;
            i += 1;
            continue;
        }
        if a == "--empty" {
            parsed.empty = true;
            i += 1;
            continue;
        }
        if let Some(v) = strip_flag(a, "--name") {
            parsed.name = Some(v.to_owned());
            i += 1;
            continue;
        }
        if a == "--name" {
            parsed.name = Some(need_value(argv, &mut i, "--name")?);
            continue;
        }
        if let Some(value) = strip_flag(a, "--lock-timeout-ms") {
            parsed.lock_timeout_ms = parse_lock_timeout(value)?;
            i += 1;
            continue;
        }
        if a == "--lock-timeout-ms" {
            let value = need_value(argv, &mut i, "--lock-timeout-ms")?;
            parsed.lock_timeout_ms = parse_lock_timeout(&value)?;
            continue;
        }
        if let Some(v) = strip_flag(a, "--steps") {
            parsed.steps = Some(parse_steps(v)?);
            i += 1;
            continue;
        }
        if a == "--steps" {
            parsed.steps = Some(parse_steps(&need_value(argv, &mut i, "--steps")?)?);
            continue;
        }
        if a.starts_with('-') {
            return Err(MigrationError::usage(format!("unknown flag `{a}`")));
        }
        if parsed.command.is_none() {
            parsed.command = Some(a.clone());
        } else {
            parsed.positional.push(a.clone());
        }
        i += 1;
    }
    Ok(parsed)
}

fn strip_flag<'a>(arg: &'a str, flag: &str) -> Option<&'a str> {
    let prefix = format!("{flag}=");
    arg.strip_prefix(&prefix)
}

fn need_value(argv: &[String], i: &mut usize, flag: &str) -> Result<String, MigrationError> {
    let value = argv
        .get(*i + 1)
        .cloned()
        .ok_or_else(|| MigrationError::usage(format!("{flag} requires a value")))?;
    *i += 2;
    Ok(value)
}

fn parse_steps(v: &str) -> Result<u32, MigrationError> {
    v.parse()
        .map_err(|_| MigrationError::usage(format!("invalid --steps `{v}`")))
}

fn parse_lock_timeout(value: &str) -> Result<u64, MigrationError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|timeout| *timeout > 0)
        .ok_or_else(|| MigrationError::usage("lock timeout must be positive"))
}

fn print_help() {
    println!(
        "\
siderite migrations

Commands:
  makemigrations [--name SLUG] [--empty] [--dry-run]
  migrate [TARGET] [--dry-run]
  rollback [--steps N | TARGET] [--dry-run]
  showmigrations
  inspectmigrations             Read-only recovery report (JSON)
  squashmigrations FROM TO [--name SLUG]

Flags:
  --lock-timeout-ms N Advisory-lock deadline (default: 30000)
  --dry-run          Print SQL / operations without writing
  --name SLUG        Slug for a new or squashed migration
  --empty            Write an empty migration
  --steps N          Number of migrations to roll back
  --help             Show this help
"
    );
}

fn cmd_makemigrations(
    models: &[&'static ModelMeta],
    dir: &Path,
    parsed: &Parsed,
) -> Result<ExitCode, MigrationError> {
    let loaded = loader::load_dir(dir)?;
    let graph = if loaded.is_empty() {
        None
    } else {
        Some(MigrationGraph::build(loaded.clone())?)
    };
    let from = match &graph {
        Some(g) => g.project_state()?,
        None => ProjectState::new(),
    };
    let operations = if parsed.empty {
        Vec::new()
    } else {
        let to = ProjectState::from_metas(models);
        autodetector::diff(&from, &to)?
    };
    if operations.is_empty() && !parsed.empty {
        println!("No changes detected.");
        return Ok(ExitCode::SUCCESS);
    }
    let is_first = loaded.is_empty();
    let slug = match &parsed.name {
        Some(n) => sanitize_slug(n),
        None => slug_from_operations(&operations, is_first),
    };
    let deps = graph
        .as_ref()
        .and_then(|g| g.order.last().cloned())
        .into_iter()
        .collect();
    let id = next_id(&loaded, &slug);
    let migration = Migration::new(id, deps, operations, true, Vec::new())?;
    if parsed.dry_run {
        println!("Would create {}:", migration.id);
        for op in &migration.operations {
            println!("  {}", op.summary());
        }
        return Ok(ExitCode::SUCCESS);
    }
    let path = loader::write_migration(dir, &migration)?;
    println!("Created {}", path.display());
    for op in &migration.operations {
        println!("  {}", op.summary());
    }
    Ok(ExitCode::SUCCESS)
}

async fn cmd_migrate(db: &Db, dir: &Path, parsed: &Parsed) -> Result<ExitCode, MigrationError> {
    let graph = load_graph(dir)?;
    let timeout = std::time::Duration::from_millis(parsed.lock_timeout_ms);
    let migrator = Migrator::new(db, &graph).with_lock_timeout(timeout)?;
    let target = parsed.positional.first().map(String::as_str);
    let report = migrator.migrate(target, parsed.dry_run).await;
    let report = name_lock_holder(&migrator, report).await?;
    print_report("Applying", &report);
    Ok(ExitCode::SUCCESS)
}

async fn cmd_rollback(db: &Db, dir: &Path, parsed: &Parsed) -> Result<ExitCode, MigrationError> {
    let graph = load_graph(dir)?;
    let timeout = std::time::Duration::from_millis(parsed.lock_timeout_ms);
    let migrator = Migrator::new(db, &graph).with_lock_timeout(timeout)?;
    let target = parsed.positional.first().map(String::as_str);
    let report = migrator
        .rollback(target, parsed.steps, parsed.dry_run)
        .await;
    let report = name_lock_holder(&migrator, report).await?;
    print_report("Unapplying", &report);
    Ok(ExitCode::SUCCESS)
}

/// On [`MigrationError::LockTimeout`], print the session holding the lock
/// to stderr; the error itself is returned unchanged.
async fn name_lock_holder(
    migrator: &Migrator<'_>,
    outcome: Result<Report, MigrationError>,
) -> Result<Report, MigrationError> {
    if matches!(outcome, Err(MigrationError::LockTimeout)) {
        match migrator.lock_holder().await {
            Ok(Some(holder)) => {
                eprintln!("the migration lock is held by {holder}");
            }
            Ok(None) => eprintln!("the migration lock has since been freed"),
            Err(err) => eprintln!("cannot identify the lock holder: {err}"),
        }
    }
    outcome
}

async fn cmd_show(db: &Db, dir: &Path) -> Result<ExitCode, MigrationError> {
    let graph = load_graph(dir)?;
    let rows = Migrator::new(db, &graph).show().await?;
    print!("{}", render_status(&rows));
    Ok(ExitCode::SUCCESS)
}

/// The `showmigrations` text for rows of [`Migrator::show`]: one
/// `[X] id` (applied) or `[ ] id` (pending) line per migration.
///
/// Args:
///   rows: `(id, applied)` pairs in display order.
///
/// Returns:
///   The text, one line per row (empty for no rows).
pub fn render_status(rows: &[(String, bool)]) -> String {
    let mut out = String::new();
    for (id, applied) in rows {
        let mark = if *applied { 'X' } else { ' ' };
        out.push_str(&format!("[{mark}] {id}\n"));
    }
    out
}

async fn cmd_inspect(db: &Db, dir: &Path) -> Result<ExitCode, MigrationError> {
    let graph = load_graph(dir)?;
    let report = Migrator::new(db, &graph).inspect_recovery().await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    eprintln!(
        "Recorded progress is not proof of the live schema. \
        Reads may race with an active migrator; reconcile catalogs before repair."
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_squash(dir: &Path, parsed: &Parsed) -> Result<ExitCode, MigrationError> {
    if parsed.positional.len() < 2 {
        return Err(MigrationError::usage(
            "squashmigrations requires FROM and TO ids",
        ));
    }
    let graph = load_graph(dir)?;
    let migration = squash::squash(
        &graph,
        &parsed.positional[0],
        &parsed.positional[1],
        parsed.name.as_deref(),
    )?;
    if parsed.dry_run {
        println!(
            "Would create {} replacing {:?}:",
            migration.id, migration.replaces
        );
        for op in &migration.operations {
            println!("  {}", op.summary());
        }
        return Ok(ExitCode::SUCCESS);
    }
    let path = loader::write_migration(dir, &migration)?;
    println!(
        "Created {} (replaces {:?})",
        path.display(),
        migration.replaces
    );
    Ok(ExitCode::SUCCESS)
}

fn load_graph(dir: &Path) -> Result<MigrationGraph, MigrationError> {
    MigrationGraph::build(loader::load_dir(dir)?)
}

fn print_report(verb: &str, report: &Report) {
    if report.planned.is_empty() {
        println!("No migrations to apply.");
        return;
    }
    if report.dry_run {
        println!("Would run:");
        for id in &report.planned {
            println!("  {id}");
        }
        for sql in &report.sql {
            println!("{sql};");
        }
        return;
    }
    for id in &report.applied {
        println!("{verb} {id}... OK");
    }
}

/// Generate a migration from compiled metadata without going through argv.
///
/// # Errors
/// Graph, IO or JSON errors.
pub fn make_migrations(
    models: &[&'static ModelMeta],
    dir: &Path,
    name: Option<&str>,
    empty: bool,
) -> Result<Option<Migration>, MigrationError> {
    let loaded = loader::load_dir(dir)?;
    let graph = if loaded.is_empty() {
        None
    } else {
        Some(MigrationGraph::build(loaded.clone())?)
    };
    let from = match &graph {
        Some(g) => g.project_state()?,
        None => ProjectState::new(),
    };
    let operations = if empty {
        Vec::new()
    } else {
        autodetector::diff(&from, &ProjectState::from_metas(models))?
    };
    if operations.is_empty() && !empty {
        return Ok(None);
    }
    let slug = match name {
        Some(n) => sanitize_slug(n),
        None => slug_from_operations(&operations, loaded.is_empty()),
    };
    let deps = graph
        .as_ref()
        .and_then(|g| g.order.last().cloned())
        .into_iter()
        .collect();
    let id = next_id(&loaded, &slug);
    let migration = Migration::new(id, deps, operations, true, Vec::new())?;
    loader::write_migration(dir, &migration)?;
    Ok(Some(migration))
}
