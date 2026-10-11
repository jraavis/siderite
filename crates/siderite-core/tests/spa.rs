//! Single-page application mounts: files, the navigation fallback and
//! what never falls back.

#![allow(clippy::unwrap_used)]

use ::http::{Method, Request, header};
use siderite_core::*;
use siderite_testkit::{TestClient, TestResponse};
use std::path::{Path, PathBuf};

fn site(name: &str, with_index: bool) -> PathBuf {
    let id = std::process::id();
    let root = std::env::temp_dir().join(format!("siderite-spa-{name}-{id}"));
    let _ = std::fs::remove_dir_all(&root);
    let dist = root.join("dist");
    std::fs::create_dir_all(dist.join("assets")).unwrap();
    if with_index {
        std::fs::write(dist.join("index.html"), "<html>app</html>").unwrap();
    }
    std::fs::write(dist.join("assets/app.js"), "console.log(1)").unwrap();
    std::fs::write(root.join("secret.txt"), "SECRET").unwrap();
    dist
}

async fn send(c: &TestClient, m: Method, path: &str, accept: &str) -> Got {
    let request = Request::builder()
        .method(m)
        .uri(path)
        .header(header::ACCEPT, accept)
        .body(Body::empty())
        .unwrap();
    c.send(request).await.unwrap()
}

type Got = TestResponse;

const HTML: &str = "text/html,application/xhtml+xml,*/*;q=0.8";
const JSON: &str = "application/json";

fn mounted(dist: &Path) -> TestClient {
    let app = App::new()
        .route("/app/api/status", get(|| async { "explicit" }))
        .spa("/app", dist);
    TestClient::new(app)
}

#[tokio::test]
async fn files_and_navigations_are_served() {
    let dist = site("serve", true);
    let client = mounted(&dist);
    let asset = send(&client, Method::GET, "/app/assets/app.js", JSON).await;
    assert_eq!(
        (asset.status.as_u16(), asset.text()),
        (200, "console.log(1)".into())
    );
    for path in ["/app", "/app/", "/app/settings", "/app/users/42/edit"] {
        let page = send(&client, Method::GET, path, HTML).await;
        assert_eq!(page.status, 200, "{path}");
        assert_eq!(page.text(), "<html>app</html>", "{path}");
    }
    let deep = send(&client, Method::GET, "/app/settings", HTML).await;
    assert_eq!(deep.headers.get(header::CACHE_CONTROL).unwrap(), "no-cache");
    let head = send(&client, Method::HEAD, "/app/settings", HTML).await;
    assert_eq!(head.status, 200);
    assert!(head.text().is_empty());
    let explicit = send(&client, Method::GET, "/app/api/status", HTML).await;
    assert_eq!(explicit.text(), "explicit", "explicit routes win");
}

#[tokio::test]
async fn misses_keep_their_meaning() {
    let dist = site("misses", true);
    let client = mounted(&dist);
    let gone = "/app/assets/gone.js";
    let missing_asset = send(&client, Method::GET, gone, HTML).await;
    assert_eq!(missing_asset.status, 404, "a missing file is not the app");
    let api = send(&client, Method::GET, "/app/settings", JSON).await;
    assert_eq!(api.status, 404, "only HTML navigations fall back");
    assert_eq!(api.content_type(), Some("application/problem+json"));
    let post = send(&client, Method::POST, "/app/settings", HTML).await;
    assert_eq!(post.status, 405);
    assert_eq!(post.headers.get(header::ALLOW).unwrap(), "GET, HEAD");
    let outside = send(&client, Method::GET, "/other", HTML).await;
    assert_eq!(outside.status, 404, "nothing outside the prefix");
}

#[tokio::test]
async fn nothing_outside_the_directory_is_reachable() {
    let dist = site("escape", true);
    #[cfg(unix)]
    {
        let target = dist.join("../secret.txt");
        std::os::unix::fs::symlink(target, dist.join("leak.txt")).unwrap();
    }
    let client = mounted(&dist);
    for path in [
        "/app/../secret.txt",
        "/app/assets/../../secret.txt",
        "/app/%2e%2e/secret.txt",
        "/app/%2E%2E%2Fsecret.txt",
        "/app/assets%5C..%5C..%5Csecret.txt",
        "/app/leak.txt",
        "/app/%zz",
    ] {
        for accept in [HTML, JSON] {
            let response = send(&client, Method::GET, path, accept).await;
            assert!(
                !response.text().contains("SECRET"),
                "{path}: {}",
                response.text()
            );
        }
        let plain = send(&client, Method::GET, path, JSON).await;
        assert_eq!(plain.status, 404, "{path}");
    }
}

#[tokio::test]
async fn a_root_mount_falls_back_only_for_navigations() {
    let dist = site("root", true);
    let app = App::new()
        .route("/api/items", get(|| async { "items" }))
        .spa("/", &dist);
    let client = TestClient::new(app);
    assert_eq!(
        send(&client, Method::GET, "/api/items", JSON).await.text(),
        "items"
    );
    let unknown_api = send(&client, Method::GET, "/api/nope", JSON).await;
    assert_eq!(unknown_api.status, 404);
    assert_eq!(unknown_api.content_type(), Some("application/problem+json"));
    let page = send(&client, Method::GET, "/dashboard", HTML).await;
    assert_eq!(page.text(), "<html>app</html>");
    let root = send(&client, Method::GET, "/", HTML).await;
    assert_eq!(root.text(), "<html>app</html>");
}

#[tokio::test]
async fn a_missing_index_is_a_404() {
    let dist = site("no-index", false);
    let client = mounted(&dist);
    let page = send(&client, Method::GET, "/app/settings", HTML).await;
    assert_eq!(page.status, 404);
    assert_eq!(page.content_type(), Some("application/problem+json"));
    let asset = send(&client, Method::GET, "/app/assets/app.js", HTML).await;
    assert_eq!(asset.status, 200);
}

#[test]
fn the_prefix_must_be_absolute() {
    let app = App::new().spa("app", "dist");
    assert!(app.into_router_service().is_err());
}
