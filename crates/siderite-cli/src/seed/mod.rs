//! `seed --dataset NAME` and `seed --list`: datasets the application
//! registers with [`AppCli::seed`](crate::AppCli::seed), loaded only when
//! asked for, into the database the global flags select.
//!
//! A seed is not a migration: nothing is recorded in the migration
//! history and there is no rollback. Nothing is deleted or reset first,
//! and no SQL file is read; the callback decides what to write. The
//! dataset runs in one transaction whenever the backend has transactions
//! (SQLite, PostgreSQL and MySQL do), so a failing seed leaves nothing
//! behind. Datasets should be idempotent (look up before inserting, or
//! rely on unique keys) so running one twice is harmless.

use crate::error::CliError;
use crate::report::{Diagnostic, Envelope, Severity, UrlSource, redact_text};
use serde::Serialize;
use siderite_orm::{Db, TransactionSupport};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[cfg(test)]
mod tests;

/// What a seed callback returns on failure; the message is redacted
/// before it is shown.
pub type SeedError = Box<dyn std::error::Error + Send + Sync>;

/// The future a registered dataset returns.
pub(crate) type SeedFuture = Pin<Box<dyn Future<Output = Seeded> + Send>>;

type Seeded = Result<(), SeedError>;

type Callback = Arc<dyn Fn(Seeding) -> SeedFuture + Send + Sync>;

/// What a dataset receives.
#[derive(Debug, Clone)]
pub struct Seeding {
    /// The selected database; inside the dataset's transaction when the
    /// backend has transactions.
    pub db: Db,
    /// The selected alias (`default` unless `--database` names another).
    pub alias: String,
    /// The dataset's name.
    pub dataset: String,
}

/// A registered dataset.
#[derive(Clone)]
pub(crate) struct Dataset {
    name: String,
    summary: String,
    run: Callback,
}

impl std::fmt::Debug for Dataset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dataset")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Dataset {
    /// The dataset `name` described by `summary`, loaded by `run`.
    pub(crate) fn new<F, Fut>(name: &str, summary: &str, run: F) -> Self
    where
        F: Fn(Seeding) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), SeedError>> + Send + 'static,
    {
        let run: Callback = Arc::new(move |s| Box::pin(run(s)));
        Self {
            name: name.to_owned(),
            summary: summary.to_owned(),
            run,
        }
    }
}

/// Parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Request {
    /// `--list`.
    List,
    /// `--dataset NAME`.
    Load(String),
}

const USAGE: &str = "usage: seed --dataset NAME | seed --list";

/// Parse the arguments after the global flags (`seed ...`).
///
/// Errors:
///   A usage error unless exactly one of `--dataset NAME` and `--list`
///   is given.
pub(crate) fn parse(rest: &[String]) -> Result<Request, CliError> {
    let mut args = rest.iter().skip(1).map(String::as_str);
    let request = match (args.next(), args.next(), args.next()) {
        (Some("--list"), None, _) => Request::List,
        (Some("--dataset"), Some(name), None) if !name.starts_with('-') => {
            Request::Load(name.to_owned())
        }
        (Some(flag), None, _) if flag.starts_with("--dataset=") => {
            Request::Load(flag["--dataset=".len()..].to_owned())
        }
        _ => return Err(CliError::usage(USAGE)),
    };
    if request == Request::Load(String::new()) {
        return Err(CliError::usage(USAGE));
    }
    Ok(request)
}

/// The registered names, in registration order.
fn names(datasets: &[Dataset]) -> String {
    let names: Vec<&str> = datasets.iter().map(|d| d.name.as_str()).collect();
    if names.is_empty() {
        "none is registered".to_owned()
    } else {
        format!("registered: {}", names.join(", "))
    }
}

type Found<'a> = Result<&'a Dataset, CliError>;

/// The dataset called `name`.
///
/// Errors:
///   A usage error naming the registered datasets when there is none,
///   or more than one, called `name`.
pub(crate) fn find<'a>(datasets: &'a [Dataset], name: &str) -> Found<'a> {
    let mut found = datasets.iter().filter(|d| d.name == name);
    match (found.next(), found.next()) {
        (Some(dataset), None) => Ok(dataset),
        (Some(_), Some(_)) => Err(CliError::usage(format!(
            "dataset `{name}` is registered more than once"
        ))),
        _ => Err(CliError::usage(format!(
            "no dataset `{name}` ({})",
            names(datasets)
        ))),
    }
}

/// One dataset in `seed --list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DatasetInfo {
    /// Its name.
    pub name: String,
    /// What it loads.
    pub summary: String,
}

/// A loaded dataset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Loaded {
    /// The dataset.
    pub dataset: String,
    /// The database alias.
    pub database: String,
    /// Where its URL came from.
    pub url_source: UrlSource,
    /// The URL without credentials.
    pub url: String,
    /// Whether it ran in one transaction.
    pub transaction: bool,
}

/// The `data` of a `seed` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SeedData {
    /// The registered datasets.
    pub datasets: Vec<DatasetInfo>,
    /// The dataset that was loaded, for `--dataset`.
    pub loaded: Option<Loaded>,
}

/// The `seed --list` report.
pub(crate) fn list(datasets: &[Dataset]) -> Envelope<SeedData> {
    let info = datasets.iter().map(|d| DatasetInfo {
        name: d.name.clone(),
        summary: d.summary.clone(),
    });
    let data = SeedData {
        datasets: info.collect(),
        loaded: None,
    };
    Envelope::new("seed", data, Vec::new())
}

/// Where a dataset is loaded.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    /// The connected database.
    pub db: Db,
    /// Its alias.
    pub alias: String,
    /// Where its URL came from.
    pub source: UrlSource,
    /// Its URL (redacted before it is reported).
    pub url: String,
}

/// Load `dataset` into `target`, in one transaction when the backend has
/// transactions.
///
/// Returns:
///   The report; a failing dataset is a `seed.E001` error diagnostic.
pub(crate) async fn load(
    datasets: &[Dataset],
    dataset: &Dataset,
    target: Target,
) -> Envelope<SeedData> {
    let support = target.db.capabilities().transactions;
    let transaction = support != TransactionSupport::None;
    let seeding = |db| Seeding {
        db,
        alias: target.alias.clone(),
        dataset: dataset.name.clone(),
    };
    let run = &dataset.run;
    let outcome = if transaction {
        let body = |tx| run(seeding(tx));
        target.db.transaction(body).await
    } else {
        run(seeding(target.db.clone())).await
    };
    let diags = outcome
        .err()
        .map(|err| failed(&dataset.name, &*err, transaction));
    let mut report = list(datasets);
    report.data.loaded = Some(Loaded {
        dataset: dataset.name.clone(),
        database: target.alias.clone(),
        url_source: target.source,
        url: redact_text(&target.url),
        transaction,
    });
    let diags: Vec<Diagnostic> = diags.into_iter().collect();
    Envelope::new("seed", report.data, diags)
}

fn failed(
    name: &str,
    err: &(dyn std::error::Error + Send + Sync),
    transaction: bool,
) -> Diagnostic {
    let after = if transaction {
        "nothing was written (the transaction was rolled back)"
    } else {
        "writes made before the failure remain (no transaction)"
    };
    let message = redact_text(&format!("dataset `{name}` failed: {err}"));
    let diag = Diagnostic::new("seed.E001", Severity::Error, &message);
    diag.with_hint(after)
}

/// Text form: what was loaded where, or the registered datasets.
pub(crate) fn render(report: &Envelope<SeedData>) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let data = &report.data;
    if let Some(loaded) = &data.loaded {
        let how = if loaded.transaction {
            "in one transaction"
        } else {
            "without a transaction"
        };
        let verb = if report.ok { "loaded" } else { "failed" };
        let _ = writeln!(
            out,
            "dataset `{}` {verb} into database `{}` ({}) {how}",
            loaded.dataset, loaded.database, loaded.url
        );
    } else if data.datasets.is_empty() {
        let _ = writeln!(out, "no datasets; register one with AppCli::seed");
    }
    if data.loaded.is_none() {
        for info in &data.datasets {
            let _ = writeln!(out, "{:<20} {}", info.name, info.summary);
        }
    }
    for diag in &report.diagnostics {
        let line = crate::report::text::diagnostic_line(diag);
        let _ = writeln!(out, "{line}");
        if let Some(hint) = diag.hint() {
            let _ = writeln!(out, "  {hint}");
        }
    }
    out
}
