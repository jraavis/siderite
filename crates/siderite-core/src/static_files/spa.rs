//! A single-page application mount: files from a directory, and the
//! app's `index.html` for browser navigations to paths that are not
//! files.
//!
//! Every path is resolved inside the canonical directory (`..` segments,
//! encoded or not, are refused, and a file reached through a symbolic
//! link must also lie inside it). Only `GET` and `HEAD` are served; other
//! methods get `405`. The `index.html` fallback is for navigations only:
//! a `GET`/`HEAD` whose `Accept` names `text/html` and whose last segment
//! has no extension. Missing assets (`/app.js`), API-style requests and a
//! missing `index.html` keep the `404` problem document.

use crate::app::route_not_found;
use axum::body::Body as AxumBody;
use axum::response::{IntoResponse as _, Response};
use http::header::{ACCEPT, ALLOW, CACHE_CONTROL};
use http::{HeaderValue, Method, Request, StatusCode};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use tower::ServiceExt;
use tower_http::services::ServeFile;

/// Serve `request` (its path already relative to the mount) from `root`.
pub(crate) async fn serve(
    root: PathBuf,
    request: Request<AxumBody>,
) -> Result<Response, Infallible> {
    let method = request.method();
    if method != Method::GET && method != Method::HEAD {
        let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
        let allow = HeaderValue::from_static("GET, HEAD");
        response.headers_mut().insert(ALLOW, allow);
        return Ok(response);
    }
    let Ok(root) = root.canonicalize() else {
        return Ok(route_not_found().await);
    };
    let Some(segments) = segments(request.uri().path()) else {
        return Ok(route_not_found().await);
    };
    if let Some(file) = existing(&root, &segments) {
        return Ok(file_response(file, request).await);
    }
    if !is_navigation(&request, &segments) {
        return Ok(route_not_found().await);
    }
    match existing(&root, &[]) {
        Some(index) => {
            let mut response = file_response(index, request).await;
            // Deploys replace it, so browsers revalidate.
            let fresh = HeaderValue::from_static("no-cache");
            response.headers_mut().insert(CACHE_CONTROL, fresh);
            Ok(response)
        }
        None => Ok(route_not_found().await),
    }
}

/// The decoded path segments, or `None` for anything that could leave
/// the directory or name something odd.
fn segments(path: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for raw in path.split('/').filter(|s| !s.is_empty()) {
        let segment = decode(raw)?;
        let dots = segment == ".." || segment == ".";
        let bad = dots || segment.contains(['/', '\\', '\0']);
        if bad {
            return None;
        }
        out.push(segment);
    }
    Some(out)
}

/// Percent-decode one segment; invalid escapes or UTF-8 are refused.
fn decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = segment.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The file `segments` names inside `root` (a directory means its
/// `index.html`), when it exists and its real path stays inside `root`.
fn existing(root: &Path, segments: &[String]) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    path.extend(segments);
    if path.is_dir() {
        path.push("index.html");
    }
    let real = path.canonicalize().ok()?;
    (real.starts_with(root) && real.is_file()).then_some(real)
}

/// A browser navigation: HTML accepted, no file extension.
fn is_navigation<B>(request: &Request<B>, segments: &[String]) -> bool {
    let accepts_html = request
        .headers()
        .get_all(ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.contains("text/html"));
    let last = segments.last().map_or("", String::as_str);
    accepts_html && !last.contains('.')
}

async fn file_response(file: PathBuf, request: Request<AxumBody>) -> Response {
    match ServeFile::new(file).oneshot(request).await {
        Ok(response) => response.map(AxumBody::new),
        Err(never) => match never {},
    }
}
