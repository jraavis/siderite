//! Static file serving, and single-page application mounts.

use crate::app::App;
use std::path::PathBuf;

mod spa;

impl App {
    /// Serve files from `dir` under URL `prefix` (for example `/assets`).
    ///
    /// The mount is not documented in OpenAPI. Path traversal (`..` segments)
    /// is rejected by `tower-http`'s `ServeDir`. A root prefix (`/` or empty)
    /// serves `dir` as the fallback for every path no route matches.
    #[must_use]
    pub fn static_files(mut self, prefix: &str, dir: impl Into<PathBuf>) -> Self {
        let prefix = prefix.trim_end_matches('/').to_owned();
        let dir = dir.into();
        if !prefix.is_empty() && !prefix.starts_with('/') {
            self.config_errors.push(format!(
                "static files prefix `{prefix}` must start with `/`"
            ));
            return self;
        }
        if prefix.is_empty() {
            // Unknown paths that are not files still get the 404 problem document.
            let not_found =
                axum::handler::HandlerWithoutStateExt::into_service(crate::app::route_not_found);
            let service = tower_http::services::ServeDir::new(dir).fallback(not_found);
            self.fallback = Some(Box::new(move |router: axum::Router| {
                router.fallback_service(service)
            }));
        } else {
            let service = tower_http::services::ServeDir::new(dir);
            self.services.push(Box::new(move |router: axum::Router| {
                router.nest_service(&prefix, service)
            }));
        }
        self
    }

    /// Serve a single-page application from `dir` under `prefix` (`/`
    /// for the whole site): files as they are, and `dir/index.html` for
    /// browser navigations to any other path below `prefix`, so client
    /// side routes (`/app/settings`) load the app.
    ///
    /// Explicit routes still win. The fallback applies only to `GET` and
    /// `HEAD` requests accepting `text/html` whose last path segment has
    /// no extension; a missing asset, an API-style request or a missing
    /// `index.html` answer with the 404 problem document, and other
    /// methods with 405. Paths are resolved inside the canonical `dir`
    /// (no `..`, encoded or not, and no symbolic link leading out). The
    /// mount is not documented in OpenAPI.
    #[must_use]
    pub fn spa(mut self, prefix: &str, dir: impl Into<PathBuf>) -> Self {
        let prefix = prefix.trim_end_matches('/').to_owned();
        if !prefix.is_empty() && !prefix.starts_with('/') {
            self.config_errors
                .push(format!("SPA prefix `{prefix}` must start with `/`"));
            return self;
        }
        let root = dir.into();
        let serve = move |request| spa::serve(root.clone(), request);
        let service = tower::service_fn(serve);
        if prefix.is_empty() {
            self.fallback = Some(Box::new(move |router: axum::Router| {
                router.fallback_service(service)
            }));
        } else {
            self.services.push(Box::new(move |router: axum::Router| {
                router.nest_service(&prefix, service)
            }));
        }
        self
    }
}
