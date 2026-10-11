//! The Markdown form of the AI context: headings and tables a person or
//! a prompt can read. It renders the same [`Context`] as the JSON form;
//! the OpenAPI document itself is left to `openapi export`.

use super::Context;
use crate::report::snapshot::{Section, Snapshot};
use crate::report::{Diagnostic, Envelope};
use std::fmt::Write;

/// Render `report` as Markdown.
pub(crate) fn render(report: &Envelope<Context>) -> String {
    let mut out = String::new();
    let ctx = &report.data;
    let snap = &ctx.snapshot;
    let _ = writeln!(
        out,
        "# {} {}\n",
        cell(&snap.app.title),
        cell(&snap.app.version)
    );
    if let Some(description) = &snap.app.description {
        let _ = writeln!(out, "{description}\n");
    }
    let _ = writeln!(
        out,
        "Context format `{} v{}`, siderite {}.",
        snap.format, snap.format_version, snap.framework.version
    );
    source(&mut out, ctx);
    project(&mut out, snap);
    migrations(&mut out, ctx);
    notes(&mut out, &report.diagnostics);
    out
}

fn source(out: &mut String, ctx: &Context) {
    let binary = ctx.source.binary.as_deref().unwrap_or("unknown binary");
    match ctx.source.built_unix {
        Some(secs) => {
            let _ = writeln!(
                out,
                "Generated from `{binary}`, built at Unix time {secs}; \
                 regenerate after changing models or routes.\n"
            );
        }
        None => {
            let text = "(build time unknown)";
            let _ = writeln!(out, "Generated from `{binary}` {text}.\n");
        }
    }
}

/// The project sections: features, databases, models and routes.
pub(crate) fn project(out: &mut String, snap: &Snapshot) {
    features(out, snap);
    databases(out, snap);
    models(out, snap);
    routes(out, snap);
}

fn features(out: &mut String, snap: &Snapshot) {
    let features = &snap.features;
    let list = if features.is_empty() {
        "none".to_owned()
    } else {
        features.join(", ")
    };
    let _ = writeln!(out, "CLI features: {list}.\n");
}

fn databases(out: &mut String, snap: &Snapshot) {
    let _ = writeln!(out, "## Databases\n");
    if snap.databases.is_empty() {
        let _ = writeln!(out, "None configured.\n");
        return;
    }
    let _ = writeln!(out, "| Alias | Backend |\n|---|---|");
    for db in &snap.databases {
        let (alias, backend) = (cell(&db.alias), cell(&db.backend));
        let _ = writeln!(out, "| {alias} | {backend} |");
    }
    out.push('\n');
}

fn models(out: &mut String, snap: &Snapshot) {
    let _ = writeln!(out, "## Models\n");
    completeness(out, &snap.completeness.models);
    if snap.models.is_empty() {
        let _ = writeln!(out, "None registered.\n");
        return;
    }
    for model in &snap.models {
        let managed = if model.managed { "" } else { ", unmanaged" };
        let _ = writeln!(
            out,
            "### {} (table `{}`{managed})\n",
            cell(&model.name),
            cell(&model.table)
        );
        let _ = writeln!(out, "| Field | Column | Type | Null | PK |");
        let _ = writeln!(out, "|---|---|---|---|---|");
        for f in &model.fields {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                cell(&f.name),
                cell(&f.column),
                cell(&f.sql_type),
                yes(f.nullable),
                yes(f.pk)
            );
        }
        out.push('\n');
    }
}

fn routes(out: &mut String, snap: &Snapshot) {
    let _ = writeln!(out, "## Routes\n");
    completeness(out, &snap.completeness.routes);
    if snap.routes.is_empty() {
        let _ = writeln!(out, "No documented routes.\n");
        return;
    }
    let _ = writeln!(out, "| Method | Path | Operation | Summary |");
    let _ = writeln!(out, "|---|---|---|---|");
    for r in &snap.routes {
        let id = r.operation_id.as_deref().unwrap_or("-");
        let summary = r.summary.as_deref().unwrap_or("");
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            cell(&r.method),
            cell(&r.path),
            cell(id),
            cell(summary)
        );
    }
    let _ = writeln!(
        out,
        "\nHidden routes are not listed. The full OpenAPI document: \
         `siderite openapi export`.\n"
    );
}

fn migrations(out: &mut String, ctx: &Context) {
    let _ = writeln!(out, "## Migrations\n");
    let m = &ctx.migrations;
    let Some(status) = &m.status else {
        let detail = m.detail.as_deref().unwrap_or("");
        let _ = writeln!(out, "Status {}: {detail}\n", m.state);
        return;
    };
    let alias = status.alias.as_deref().unwrap_or("the database");
    let _ = writeln!(
        out,
        "Read from `{alias}`: {} applied, {} pending.\n",
        status.applied, status.pending
    );
    for row in &status.migrations {
        let mark = if row.applied { 'X' } else { ' ' };
        let _ = writeln!(out, "- [{mark}] {}", cell(&row.id));
    }
    out.push('\n');
}

fn notes(out: &mut String, diagnostics: &[Diagnostic]) {
    if diagnostics.is_empty() {
        return;
    }
    let _ = writeln!(out, "## Notes\n");
    for d in diagnostics {
        let _ = writeln!(
            out,
            "- {}: [{}] {}",
            d.severity().as_str(),
            d.code(),
            d.message()
        );
    }
    out.push('\n');
}

fn completeness(out: &mut String, section: &Section) {
    if section.complete {
        return;
    }
    let reason = section.reason.as_deref().unwrap_or("incomplete");
    let total = section
        .total
        .map_or_else(|| "an unknown number".to_owned(), |t| t.to_string());
    let _ = writeln!(
        out,
        "**Incomplete:** {} of {total} shown ({reason}).\n",
        section.included
    );
}

fn yes(flag: bool) -> &'static str {
    if flag { "yes" } else { "" }
}

/// Text safe inside a table cell or heading: no pipes or line breaks.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}
