//! Text renderers. They reproduce the historical terminal output of
//! `check` and `routes` byte for byte.

use super::{CheckSummary, Diagnostic, Envelope, RouteReport};

/// One line of a diagnostic: `error: [models.E003] message`.
pub fn diagnostic_line(d: &Diagnostic) -> String {
    format!("{}: [{}] {}", d.severity.as_str(), d.code, d.message)
}

/// Render a `check` report: one line per diagnostic, then a summary line.
pub fn render_check(report: &Envelope<CheckSummary>) -> String {
    let mut out = String::new();
    for d in &report.diagnostics {
        out.push_str(&diagnostic_line(d));
        out.push('\n');
    }
    if report.diagnostics.is_empty() {
        out.push_str("System check identified no issues.\n");
    } else {
        out.push_str(&format!(
            "System check identified {} issue(s).\n",
            report.diagnostics.len()
        ));
    }
    out
}

/// Render routes as an aligned table, one route per line.
pub fn render_routes(routes: &[RouteReport]) -> String {
    render_route_rows(routes.iter().map(|r| {
        (
            r.method.as_str(),
            r.path.as_str(),
            r.operation_id.as_deref(),
        )
    }))
}

/// The table behind [`render_routes`], over `(method, path, id)` rows.
pub(crate) fn render_route_rows<'a>(
    rows: impl Iterator<Item = (&'a str, &'a str, Option<&'a str>)> + Clone,
) -> String {
    let method_width = rows.clone().map(|r| r.0.len()).max().unwrap_or(0);
    let path_width = rows.clone().map(|r| r.1.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (method, path, id) in rows {
        let id = id.unwrap_or("-");
        out.push_str(&format!(
            "{method:<method_width$}  {path:<path_width$}  {id}\n"
        ));
    }
    if out.is_empty() {
        return "No routes.\n".to_owned();
    }
    out
}
