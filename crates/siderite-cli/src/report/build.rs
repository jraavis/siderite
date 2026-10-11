//! Pure report builders. They read compiled metadata only: no database
//! connection is opened and nothing is printed.

use super::{CheckSummary, Diagnostic, Envelope, Severity, redact_text};
use super::{FieldReport, ModelReport, ModelsData};
use super::{MigrationReport, MigrationStatusData, UrlSource};
use super::{RouteReport, RoutesData};
use crate::check::{CheckIssue, CheckLevel, check, check_build};
use crate::error::CliError;
use crate::settings::CliSettings;
use siderite_core::App;
use siderite_orm::ModelMeta;
use std::path::Path;

/// The documented routes of `app`, sorted by path then method.
///
/// Rows come from the OpenAPI document, so hidden endpoints are omitted.
///
/// Errors:
///   [`CliError::OpenApi`] when the document cannot be generated.
pub fn route_reports(app: &App) -> Result<Vec<RouteReport>, CliError> {
    let document = app
        .openapi()
        .map_err(|err| CliError::OpenApi(err.to_string()))?;
    let mut rows = Vec::new();
    for (path, item) in &document.paths {
        for (method, operation) in &item.0 {
            let mut tags = operation.tags.clone();
            tags.sort();
            tags.dedup();
            rows.push(RouteReport {
                method: method.to_ascii_uppercase(),
                path: path.clone(),
                operation_id: operation.operation_id.clone(),
                tags,
                summary: operation.summary.as_deref().map(redact_text),
            });
        }
    }
    rows.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
    Ok(rows)
}

/// The `routes` report of `app`.
///
/// Errors:
///   [`CliError::OpenApi`] when the document cannot be generated.
pub fn routes(app: &App) -> Result<Envelope<RoutesData>, CliError> {
    let routes = route_reports(app)?;
    Ok(Envelope::new("routes", RoutesData { routes }, Vec::new()))
}

/// Every registered model, sorted by name then table, fields sorted by
/// name then column.
pub fn model_reports(models: &[&'static ModelMeta]) -> Vec<ModelReport> {
    let mut reports: Vec<ModelReport> = models
        .iter()
        .map(|model| {
            let mut fields: Vec<FieldReport> = model
                .fields
                .iter()
                .map(|f| FieldReport {
                    name: f.name.to_owned(),
                    column: f.column.to_owned(),
                    sql_type: format!("{:?}", f.sql_type),
                    nullable: f.nullable,
                    pk: f.primary_key,
                })
                .collect();
            fields.sort_by_key(|f| (f.name.clone(), f.column.clone()));
            ModelReport {
                name: model.name.to_owned(),
                table: model.table.to_owned(),
                managed: model.managed,
                fields,
            }
        })
        .collect();
    reports.sort_by(|a, b| (&a.name, &a.table).cmp(&(&b.name, &b.table)));
    reports
}

/// The `models` report of the registered `models`.
pub fn models(models: &[&'static ModelMeta]) -> Envelope<ModelsData> {
    let models = model_reports(models);
    Envelope::new("models", ModelsData { models }, Vec::new())
}

/// Convert a [`CheckIssue`] to a [`Diagnostic`].
pub fn diagnostic(issue: &CheckIssue) -> Diagnostic {
    let severity = match issue.level {
        CheckLevel::Warning => Severity::Warning,
        CheckLevel::Error => Severity::Error,
    };
    Diagnostic::new(issue.id, severity, &issue.message)
}

/// The `check` report: every issue in check order (config, models,
/// migrations, OpenAPI, backend, router build).
///
/// `factory` builds a fresh app; the router-build check consumes one.
/// `migrations_dir` is where the JSON migrations live (`None` skips the
/// migration checks).
pub fn check_report(
    factory: &dyn Fn() -> App,
    models: &[&'static ModelMeta],
    settings: &CliSettings,
    migrations_dir: Option<&Path>,
) -> Envelope<CheckSummary> {
    let mut issues = check(&factory(), models, settings, migrations_dir);
    // A duplicate operation already fails the OpenAPI check; do not
    // report the same route problem a second time.
    if !issues.iter().any(|i| i.id.starts_with("openapi.")) {
        issues.extend(check_build(factory()));
    }
    let errors = issues
        .iter()
        .filter(|i| i.level == CheckLevel::Error)
        .count();
    let summary = CheckSummary {
        errors,
        warnings: issues.len() - errors,
    };
    let diagnostics = issues.iter().map(diagnostic).collect();
    Envelope::new("check", summary, diagnostics)
}

/// The `showmigrations` report over rows of `Migrator::show`, kept in
/// their order so JSON and text agree.
pub fn migration_status(
    alias: Option<&str>,
    source: UrlSource,
    rows: Vec<(String, bool)>,
) -> Envelope<MigrationStatusData> {
    let applied = rows.iter().filter(|(_, applied)| *applied).count();
    let pending = rows.len() - applied;
    let migrations = rows
        .into_iter()
        .map(|(id, applied)| MigrationReport { id, applied })
        .collect();
    let data = MigrationStatusData {
        alias: alias.map(str::to_owned),
        source,
        migrations,
        applied,
        pending,
    };
    Envelope::new("showmigrations", data, Vec::new())
}
