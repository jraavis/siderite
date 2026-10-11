//! `routes`: the table of `METHOD PATH operation_id` an app serves.
//!
//! Rows come from the app's OpenAPI document, so mounted apps appear under
//! their prefix and endpoints marked `hidden` are left out.

use crate::error::CliError;
use crate::report::build::route_reports;
use crate::report::text::render_route_rows;
use siderite_core::App;

/// One row of the route table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRow {
    /// Upper-case HTTP method.
    pub method: String,
    /// Full path template, including mount prefixes.
    pub path: String,
    /// The operation id, if the endpoint has one.
    pub operation_id: Option<String>,
}

/// Every documented route of `app`, sorted by path, then method.
///
/// # Errors
/// [`CliError::OpenApi`] when the OpenAPI document cannot be generated (for
/// example on a duplicate operation).
pub fn route_table(app: &App) -> Result<Vec<RouteRow>, CliError> {
    Ok(route_reports(app)?
        .into_iter()
        .map(|r| RouteRow {
            method: r.method,
            path: r.path,
            operation_id: r.operation_id,
        })
        .collect())
}

/// Render `rows` as an aligned text table, one route per line.
pub fn render_routes(rows: &[RouteRow]) -> String {
    render_route_rows(rows.iter().map(|r| {
        (
            r.method.as_str(),
            r.path.as_str(),
            r.operation_id.as_deref(),
        )
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use siderite_core::{get, post};

    fn app() -> App {
        let v1 = App::new()
            .route(
                "/users",
                post(|| async { "c" })
                    .operation_id("create_user")
                    .get(|| async { "l" }),
            )
            .route("/", get(|| async { "root" }));
        App::new()
            .route("/health", get(|| async { "ok" }).operation_id("health"))
            .route("/secret", get(|| async { "s" }).hidden())
            .mount("/api/v1", v1)
    }

    #[test]
    fn lists_mounts_sorted_and_skips_hidden() {
        let rows = route_table(&app()).unwrap();
        let summary: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.method.as_str(),
                    r.path.as_str(),
                    r.operation_id.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("GET", "/api/v1", None),
                ("GET", "/api/v1/users", None),
                ("POST", "/api/v1/users", Some("create_user")),
                ("GET", "/health", Some("health")),
            ]
        );
    }

    #[test]
    fn duplicate_operations_are_an_error() {
        let app = App::new()
            .route("/a", get(|| async { "a" }).operation_id("same"))
            .route("/b", get(|| async { "b" }).operation_id("same"));
        let err = route_table(&app).unwrap_err();
        assert!(err.to_string().contains("same"), "{err}");
    }

    #[test]
    fn table_is_aligned() {
        let text = render_routes(&route_table(&app()).unwrap());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2], "POST  /api/v1/users  create_user");
        assert_eq!(lines[3], "GET   /health        health");
        assert_eq!(lines[0], "GET   /api/v1        -");
    }

    #[test]
    fn empty_app_has_no_routes() {
        assert_eq!(
            render_routes(&route_table(&App::new()).unwrap()),
            "No routes.\n"
        );
    }
}
