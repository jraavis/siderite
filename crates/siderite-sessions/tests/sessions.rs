//! Session layer behaviour: cookies, ids, rotation, expiry, failures.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use siderite_core::SameSite;
use siderite_core::http::{Method, StatusCode};
use siderite_sessions::{
    MemoryStore, SessionConfig, SessionLayer, SessionStore, id_hash,
};
use siderite_testkit::TestClient;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A client over a fresh spy store with the default configuration.
fn setup() -> (TestClient, Spy) {
    let spy = Spy::new();
    (client(spy.clone(), SessionConfig::new()), spy)
}

/// Move a stored session back in time.
async fn age(spy: &Spy, cookie: &str, created: u64, last_seen: u64) {
    let hash = hash_of(cookie);
    let mut record = spy.inner.load(&hash).await.unwrap().unwrap();
    record.created_at = created;
    record.last_seen = last_seen;
    let ttl = Duration::from_secs(3600);
    spy.inner.save(&hash, &record, ttl).await.unwrap();
}

async fn login(client: &TestClient) -> String {
    pair(&post_as(client, "/login", None).await).unwrap()
}

// ---- cookie ---------------------------------------------------------------

#[tokio::test]
async fn cookie_has_the_required_flags() {
    let (client, _) = setup();
    let res = post_as(&client, "/login", None).await;
    let header = set_cookie(&res).unwrap();
    let parts: Vec<&str> = header.split("; ").collect();
    assert!(parts[0].starts_with("__Host-session="), "{header}");
    for flag in ["HttpOnly", "Secure", "Path=/", "SameSite=Lax"] {
        assert!(parts.contains(&flag), "{flag} missing: {header}");
    }
    assert!(parts.contains(&"Max-Age=43200"), "{header}");
    assert!(!header.to_ascii_lowercase().contains("domain"), "{header}");
    assert!(!header.contains("Expires"), "{header}");
}

#[tokio::test]
async fn same_site_strict_is_an_option() {
    let config = SessionConfig::new().same_site(SameSite::Strict);
    let client = client(Spy::new(), config);
    let header = set_cookie(&post_as(&client, "/login", None).await).unwrap();
    assert!(header.contains("SameSite=Strict"), "{header}");
}

#[tokio::test]
async fn insecure_development_drops_secure_and_the_prefix() {
    let config = SessionConfig::new().insecure_development();
    assert_eq!(config.name(), "session");
    let client = client(Spy::new(), config);
    let header = set_cookie(&post_as(&client, "/login", None).await).unwrap();
    assert!(header.starts_with("session="), "{header}");
    assert!(!header.contains("Secure"), "{header}");
    assert!(header.contains("HttpOnly"), "{header}");
}

#[tokio::test]
async fn insecure_is_never_inferred_from_the_request() {
    let (client, _) = setup();
    let headers = [("x-forwarded-proto", "http"), ("host", "localhost")];
    let res = send(&client, Method::POST, "/login", None, &headers).await;
    assert!(set_cookie(&res).unwrap().contains("Secure"));
}

#[test]
fn invalid_configurations_are_refused() {
    let key = vec![7u8; 32];
    let bad = [
        SessionConfig::new().cookie_name(""),
        SessionConfig::new().cookie_name("bad name"),
        SessionConfig::new().cookie_name("a;b"),
        SessionConfig::new().same_site(SameSite::None),
        SessionConfig::new().csrf_key(vec![1u8; 31]),
        SessionConfig::new().csrf_header("bad header"),
        SessionConfig::new().idle_timeout(Duration::ZERO),
        SessionConfig::new().absolute_lifetime(Duration::ZERO),
        SessionConfig::new().store_timeout(Duration::ZERO),
        SessionConfig::new().write_interval(Duration::ZERO),
        SessionConfig::new().write_interval(Duration::from_secs(3600)),
        SessionConfig::new()
            .same_site(SameSite::None)
            .csrf_key(key.clone())
            .insecure_development(),
        SessionConfig::new()
            .insecure_development()
            .cookie_name("__Host-session"),
    ];
    for config in bad {
        assert!(
            SessionLayer::new(MemoryStore::default(), config.clone()).is_err(),
            "{config:?}"
        );
    }
    let ok = SessionConfig::new().same_site(SameSite::None).csrf_key(key);
    assert!(SessionLayer::new(MemoryStore::default(), ok).is_ok());
}

#[test]
fn debug_output_hides_the_csrf_key() {
    let config = SessionConfig::new()
        .csrf_key(b"super-secret-key-of-32-bytes!!!!".to_vec());
    let text = format!("{config:?}");
    assert!(!text.contains("super-secret"), "{text}");
    assert!(text.contains("REDACTED"), "{text}");
}

// ---- ids and the store ----------------------------------------------------

#[tokio::test]
async fn ids_are_long_distinct_and_url_safe() {
    let (client, _) = setup();
    let mut seen = HashSet::new();
    for _ in 0..200 {
        let id = raw_id(&post_as(&client, "/login", None).await).unwrap();
        assert_eq!(id.len(), 43, "32 bytes in base64url: {id}");
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            "{id}"
        );
        assert!(seen.insert(id), "duplicate id");
    }
}

#[tokio::test]
async fn stores_only_ever_see_the_sha256_of_an_id() {
    let (client, spy) = setup();
    let cookie = login(&client).await;
    get_as(&client, "/me", Some(&cookie)).await;
    post_as(&client, "/rotate", Some(&cookie)).await;
    let raw = cookie.split_once('=').unwrap().1.to_owned();
    let seen = spy.seen();
    assert!(!seen.is_empty());
    for id in &seen {
        assert_eq!(id.len(), 64, "{id}");
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
        assert_ne!(*id, raw);
        assert!(!id.contains(&raw));
    }
    assert!(seen.contains(&id_hash(&raw)));
}

#[test]
fn id_hash_is_sha256_hex() {
    assert_eq!(
        id_hash("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

// ---- basic behaviour ------------------------------------------------------

#[tokio::test]
async fn anonymous_requests_store_nothing_and_set_no_cookie() {
    let (client, spy) = setup();
    let res = get_as(&client, "/me", None).await;
    assert_eq!(res.text(), "anonymous");
    assert!(set_cookies(&res).is_empty());
    assert_eq!(spy.saves.load(Ordering::SeqCst), 0);
    assert_eq!(spy.inner.len(), 0);
}

#[tokio::test]
async fn data_round_trips_through_the_cookie() {
    let (client, _) = setup();
    let res = post_as(&client, "/set", None).await;
    let cookie = pair(&res).unwrap();
    let res = get_as(&client, "/get", Some(&cookie)).await;
    assert_eq!(res.text(), "red");
    assert!(set_cookies(&res).is_empty(), "cookie is not re-sent");
    let res = get_as(&client, "/get", None).await;
    assert_eq!(res.text(), "none");
}

#[tokio::test]
async fn login_identifies_the_user() {
    let (client, _) = setup();
    let cookie = login(&client).await;
    let res = get_as(&client, "/me", Some(&cookie)).await;
    assert_eq!(res.text(), "ada");
}

// ---- rotation and revocation ----------------------------------------------

#[tokio::test]
async fn login_rotates_the_id_and_deletes_the_old_one_at_once() {
    let (client, spy) = setup();
    let first = pair(&post_as(&client, "/set", None).await).unwrap();
    let old = hash_of(&first);
    assert!(spy.inner.load(&old).await.unwrap().is_some());

    let res = post_as(&client, "/login", Some(&first)).await;
    let second = pair(&res).unwrap();
    assert_ne!(first, second);
    assert!(
        spy.inner.load(&old).await.unwrap().is_none(),
        "old id lives"
    );
    assert_eq!(
        get_as(&client, "/me", Some(&first)).await.text(),
        "anonymous"
    );
    assert_eq!(get_as(&client, "/me", Some(&second)).await.text(), "ada");
    // Data collected before login survives it.
    assert_eq!(get_as(&client, "/get", Some(&second)).await.text(), "red");
    assert_eq!(spy.inner.len(), 1);
}

#[tokio::test]
async fn login_as_another_user_drops_the_old_data() {
    let (client, _) = setup();
    let ada = login(&client).await;
    let set = post_as(&client, "/set", Some(&ada)).await;
    assert!(set_cookies(&set).is_empty());
    assert_eq!(get_as(&client, "/get", Some(&ada)).await.text(), "red");
    let bob = pair(&post_as(&client, "/login-bob", Some(&ada)).await).unwrap();
    assert_eq!(get_as(&client, "/me", Some(&bob)).await.text(), "bob");
    assert_eq!(get_as(&client, "/get", Some(&bob)).await.text(), "none");
}

#[tokio::test]
async fn rotate_replaces_the_id_and_keeps_the_data() {
    let (client, spy) = setup();
    let first = pair(&post_as(&client, "/set", None).await).unwrap();
    let second =
        pair(&post_as(&client, "/rotate", Some(&first)).await).unwrap();
    assert_ne!(first, second);
    assert!(spy.inner.load(&hash_of(&first)).await.unwrap().is_none());
    assert_eq!(get_as(&client, "/get", Some(&second)).await.text(), "red");
    assert_eq!(get_as(&client, "/get", Some(&first)).await.text(), "none");
}

#[tokio::test]
async fn rotation_does_not_extend_the_absolute_lifetime() {
    let (client, spy) = setup();
    let first = login(&client).await;
    age(&spy, &first, now() - 40_000, now()).await;
    let second =
        pair(&post_as(&client, "/rotate", Some(&first)).await).unwrap();
    let record = spy.inner.load(&hash_of(&second)).await.unwrap().unwrap();
    assert!(record.created_at <= now() - 40_000);
}

#[tokio::test]
async fn logout_deletes_the_session_and_expires_the_cookie() {
    let (client, spy) = setup();
    let cookie = login(&client).await;
    let res = post_as(&client, "/logout", Some(&cookie)).await;
    let header = set_cookie(&res).unwrap();
    assert!(header.starts_with("__Host-session=;"), "{header}");
    assert!(header.contains("Max-Age=0"), "{header}");
    for flag in ["HttpOnly", "Secure", "Path=/"] {
        assert!(header.contains(flag), "{header}");
    }
    assert!(spy.inner.load(&hash_of(&cookie)).await.unwrap().is_none());
    assert_eq!(
        get_as(&client, "/me", Some(&cookie)).await.text(),
        "anonymous"
    );
    assert_eq!(spy.inner.len(), 0);
}

#[tokio::test]
async fn revoking_a_user_ends_all_their_sessions() {
    let (client, spy) = setup();
    let first = login(&client).await;
    let second = login(&client).await;
    let bob = pair(&post_as(&client, "/login-bob", None).await).unwrap();
    spy.revoke_user("ada").await.unwrap();
    for cookie in [&first, &second] {
        let res = get_as(&client, "/me", Some(cookie)).await;
        assert_eq!(res.text(), "anonymous");
    }
    assert_eq!(get_as(&client, "/me", Some(&bob)).await.text(), "bob");
    // A fresh login after the revocation works.
    let again = login(&client).await;
    assert_eq!(get_as(&client, "/me", Some(&again)).await.text(), "ada");
}

#[tokio::test]
async fn a_racing_write_cannot_revive_a_revoked_session() {
    let (client, spy) = setup();
    let cookie = login(&client).await;
    let hash = hash_of(&cookie);
    let stale = spy.inner.load(&hash).await.unwrap().unwrap();
    spy.revoke_user("ada").await.unwrap();
    let ttl = Duration::from_secs(60);
    assert!(spy.inner.save(&hash, &stale, ttl).await.is_err());
    assert!(spy.inner.load(&hash).await.unwrap().is_none());
}

// ---- expiry ---------------------------------------------------------------

#[tokio::test]
async fn idle_timeout_is_enforced_on_the_server() {
    let (client, spy) = setup();
    let cookie = login(&client).await;
    let t = now();
    age(&spy, &cookie, t - 60, t - 31 * 60).await;
    let res = get_as(&client, "/me", Some(&cookie)).await;
    assert_eq!(res.text(), "anonymous");
    let header = set_cookie(&res).unwrap();
    assert!(header.contains("Max-Age=0"), "cookie is cleared: {header}");
    assert!(spy.inner.load(&hash_of(&cookie)).await.unwrap().is_none());
    // Just inside the timeout the session is still good.
    let cookie = login(&client).await;
    age(&spy, &cookie, t - 60, t - 29 * 60).await;
    assert_eq!(get_as(&client, "/me", Some(&cookie)).await.text(), "ada");
}

#[tokio::test]
async fn absolute_lifetime_is_enforced_however_active_the_session_is() {
    let (client, spy) = setup();
    let cookie = login(&client).await;
    let t = now();
    age(&spy, &cookie, t - 13 * 3600, t).await;
    let res = get_as(&client, "/me", Some(&cookie)).await;
    assert_eq!(res.text(), "anonymous");
    assert!(spy.inner.load(&hash_of(&cookie)).await.unwrap().is_none());
    let cookie = login(&client).await;
    age(&spy, &cookie, t - 11 * 3600, t).await;
    assert_eq!(get_as(&client, "/me", Some(&cookie)).await.text(), "ada");
}

#[tokio::test]
async fn custom_lifetimes_apply() {
    let spy = Spy::new();
    let config = SessionConfig::new()
        .idle_timeout(Duration::from_secs(60))
        .absolute_lifetime(Duration::from_secs(120));
    let client = client(spy.clone(), config);
    let cookie = login(&client).await;
    let t = now();
    age(&spy, &cookie, t - 100, t - 61).await;
    assert_eq!(
        get_as(&client, "/me", Some(&cookie)).await.text(),
        "anonymous"
    );
    let cookie = login(&client).await;
    age(&spy, &cookie, t - 121, t).await;
    assert_eq!(
        get_as(&client, "/me", Some(&cookie)).await.text(),
        "anonymous"
    );
}

#[tokio::test]
async fn idle_writes_are_throttled() {
    let (client, spy) = setup();
    let cookie = login(&client).await;
    let saves = || spy.saves.load(Ordering::SeqCst);
    let before = saves();
    for _ in 0..5 {
        get_as(&client, "/me", Some(&cookie)).await;
    }
    assert_eq!(saves(), before, "reads must not become writes");

    // Past a quarter of the idle timeout (450 s) one request writes.
    let t = now();
    age(&spy, &cookie, t - 1000, t - 500).await;
    let before = saves();
    assert_eq!(get_as(&client, "/me", Some(&cookie)).await.text(), "ada");
    assert_eq!(saves(), before + 1);
    let record = spy.inner.load(&hash_of(&cookie)).await.unwrap().unwrap();
    assert!(record.last_seen >= t - 1, "last_seen was refreshed");
    get_as(&client, "/me", Some(&cookie)).await;
    assert_eq!(saves(), before + 1, "and only once");
}

#[tokio::test]
async fn a_failed_idle_write_does_not_fail_the_request() {
    let (client, spy) = setup();
    let cookie = login(&client).await;
    let t = now();
    age(&spy, &cookie, t - 1000, t - 500).await;
    Spy::flag(&spy.fail_save, true);
    let res = get_as(&client, "/me", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.text(), "ada");
}

#[tokio::test]
async fn unknown_and_malformed_cookies_are_anonymous_and_cleared() {
    let (client, spy) = setup();
    let unknown = format!("__Host-session={}", "A".repeat(43));
    let res = get_as(&client, "/me", Some(&unknown)).await;
    assert_eq!(res.text(), "anonymous");
    assert!(set_cookie(&res).unwrap().contains("Max-Age=0"));

    let loads = spy.loads.load(Ordering::SeqCst);
    for junk in [
        "__Host-session=short",
        "__Host-session=",
        "__Host-session=%00",
    ] {
        let res = get_as(&client, "/me", Some(junk)).await;
        assert_eq!(res.text(), "anonymous", "{junk}");
        assert!(set_cookie(&res).unwrap().contains("Max-Age=0"));
    }
    let long = format!("__Host-session={}", "A".repeat(44));
    assert_eq!(
        get_as(&client, "/me", Some(&long)).await.text(),
        "anonymous"
    );
    assert_eq!(
        spy.loads.load(Ordering::SeqCst),
        loads,
        "no lookups for junk"
    );
    // Another cookie's name is not the session.
    let other = format!("other={}", "A".repeat(43));
    let res = get_as(&client, "/me", Some(&other)).await;
    assert!(set_cookies(&res).is_empty());
}

// ---- failure behaviour ----------------------------------------------------

#[tokio::test]
async fn a_failed_read_fails_closed_with_503() {
    let (client, spy) = setup();
    let cookie = login(&client).await;
    Spy::flag(&spy.fail_load, true);
    let res = get_as(&client, "/me", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    let body = problem_body(&res);
    assert_eq!(body["status"], 503);
    assert!(set_cookies(&res).is_empty());
    let text = res.text();
    assert!(!text.contains("injected"), "store detail leaked: {text}");
    // A route that never extracts the session fails too: the layer decided.
    let res = get_as(&client, "/anon", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    // Without a cookie there is nothing to read, so the request proceeds.
    let res = get_as(&client, "/me", None).await;
    assert_eq!(res.text(), "anonymous");
}

#[tokio::test]
async fn a_failed_login_write_sets_no_cookie() {
    let (client, spy) = setup();
    Spy::flag(&spy.fail_save, true);
    let res = post_as(&client, "/login", None).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(set_cookies(&res).is_empty());
    assert_eq!(spy.inner.len(), 0);
    let res = post_as(&client, "/set", None).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(set_cookies(&res).is_empty());
}

#[tokio::test]
async fn a_failed_change_or_rotation_write_fails_the_request() {
    let (client, spy) = setup();
    let cookie = login(&client).await;
    Spy::flag(&spy.fail_save, true);
    let res = post_as(&client, "/set", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    let res = post_as(&client, "/rotate", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(set_cookies(&res).is_empty());
    Spy::flag(&spy.fail_save, false);
    // The old session is still the only one and still works.
    assert_eq!(get_as(&client, "/me", Some(&cookie)).await.text(), "ada");
    assert_eq!(spy.inner.len(), 1);
}

#[tokio::test]
async fn failing_to_delete_the_old_id_fails_rotation_without_a_cookie() {
    let (client, spy) = setup();
    let cookie = login(&client).await;
    Spy::flag(&spy.fail_delete, true);
    let res = post_as(&client, "/login", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(set_cookies(&res).is_empty());
    let res = post_as(&client, "/logout", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(set_cookies(&res).is_empty(), "logout failed, cookie stays");
}

#[tokio::test]
async fn store_calls_are_bounded_by_the_timeout() {
    let spy = Spy::new();
    let config = SessionConfig::new().store_timeout(Duration::from_millis(60));
    let client = client(spy.clone(), config);
    let cookie = login(&client).await;
    spy.delay_ms.store(1500, Ordering::SeqCst);

    let start = Instant::now();
    let res = get_as(&client, "/me", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(start.elapsed() < Duration::from_millis(1000));

    let start = Instant::now();
    let res = post_as(&client, "/login", None).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        set_cookies(&res).is_empty(),
        "no cookie for a timed out write"
    );
    assert!(start.elapsed() < Duration::from_millis(1000));
}

#[tokio::test]
async fn a_missing_layer_is_a_server_error() {
    let app = siderite_core::App::new().route(
        "/me",
        siderite_core::get(|s: siderite_sessions::Session| async move {
            s.user().unwrap_or_default()
        }),
    );
    let client = TestClient::new(app);
    let res = get_as(&client, "/me", None).await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
}

// ---- CSRF tokens ----------------------------------------------------------

fn csrf_client(spy: &Spy) -> TestClient {
    let key = b"0123456789abcdef0123456789abcdef".to_vec();
    client(spy.clone(), SessionConfig::new().csrf_key(key))
}

#[tokio::test]
async fn csrf_tokens_are_bound_to_the_session() {
    let spy = Spy::new();
    let client = csrf_client(&spy);
    let a = login(&client).await;
    let b = login(&client).await;
    let token_a = get_as(&client, "/token", Some(&a)).await.text();
    let token_b = get_as(&client, "/token", Some(&b)).await.text();
    assert_eq!(token_a.len(), 43, "base64url HMAC-SHA256: {token_a}");
    assert_ne!(token_a, token_b);
    assert_eq!(get_as(&client, "/token", Some(&a)).await.text(), token_a);

    assert_eq!(verify_with(&client, &a, &token_a).await, StatusCode::OK);
    assert_eq!(verify_with(&client, &b, &token_b).await, StatusCode::OK);
    assert_eq!(
        verify_with(&client, &a, &token_b).await,
        StatusCode::FORBIDDEN
    );
    let mut flipped = token_a.clone().into_bytes();
    flipped[0] = if flipped[0] == b'A' { b'B' } else { b'A' };
    let flipped = String::from_utf8(flipped).unwrap();
    for bad in [
        "",
        "x",
        "A",
        &token_a[1..],
        &format!("{token_a}A"),
        &flipped,
    ] {
        assert_eq!(
            verify_with(&client, &a, bad).await,
            StatusCode::FORBIDDEN,
            "{bad}"
        );
    }
    // No session at all: nothing to verify against.
    let headers = [("x-csrf-token", token_a.as_str())];
    let res = send(&client, Method::POST, "/verify", None, &headers).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}

async fn verify_with(
    client: &TestClient,
    cookie: &str,
    token: &str,
) -> StatusCode {
    let headers = [("x-csrf-token", token)];
    let res = send(client, Method::POST, "/verify", Some(cookie), &headers);
    res.await.status
}

#[tokio::test]
async fn a_csrf_token_creates_a_session_and_changes_on_rotation() {
    let spy = Spy::new();
    let client = csrf_client(&spy);
    let res = get_as(&client, "/token", None).await;
    let cookie = pair(&res).unwrap();
    let token = res.text();
    assert!(spy.inner.load(&hash_of(&cookie)).await.unwrap().is_some());
    let headers = [("x-csrf-token", token.as_str())];
    let res =
        send(&client, Method::POST, "/verify", Some(&cookie), &headers).await;
    assert_eq!(res.status, StatusCode::OK);

    let rotated =
        pair(&post_as(&client, "/rotate", Some(&cookie)).await).unwrap();
    let res =
        send(&client, Method::POST, "/verify", Some(&rotated), &headers).await;
    assert_eq!(
        res.status,
        StatusCode::FORBIDDEN,
        "old token after rotation"
    );
    let fresh = get_as(&client, "/token", Some(&rotated)).await.text();
    assert_ne!(fresh, token);
}

#[tokio::test]
async fn without_a_key_there_is_no_token() {
    let (client, _) = setup();
    let cookie = login(&client).await;
    let res = get_as(&client, "/token", Some(&cookie)).await;
    assert_eq!(res.text(), "no-token");
    let headers = [("x-csrf-token", "anything")];
    let res =
        send(&client, Method::POST, "/verify", Some(&cookie), &headers).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    // And asking did not create a session.
    let res = get_as(&client, "/token", None).await;
    assert!(set_cookies(&res).is_empty());
}

#[tokio::test]
async fn same_site_none_requires_the_token_on_unsafe_requests() {
    let spy = Spy::new();
    let key = b"0123456789abcdef0123456789abcdef".to_vec();
    let config = SessionConfig::new().same_site(SameSite::None).csrf_key(key);
    let client = client(spy.clone(), config);
    let res = post_as(&client, "/login", None).await;
    let cookie = pair(&res).unwrap();
    assert!(set_cookie(&res).unwrap().contains("SameSite=None"));
    let token = get_as(&client, "/token", Some(&cookie)).await.text();

    // Safe methods need no token.
    assert_eq!(get_as(&client, "/me", Some(&cookie)).await.text(), "ada");
    let cases: [(&[(&str, &str)], StatusCode); 5] = [
        (&[], StatusCode::FORBIDDEN),
        (&[("x-csrf-token", "nope")], StatusCode::FORBIDDEN),
        (&[("x-csrf-token", "")], StatusCode::FORBIDDEN),
        (
            &[("x-csrf-token", &token), ("x-csrf-token", &token)],
            StatusCode::FORBIDDEN,
        ),
        (&[("x-csrf-token", &token)], StatusCode::OK),
    ];
    for (headers, want) in cases {
        let res =
            send(&client, Method::POST, "/set", Some(&cookie), headers).await;
        assert_eq!(res.status, want, "{headers:?}");
    }
    // Without a session cookie nothing is authenticated, so no token yet.
    let res = post_as(&client, "/set", None).await;
    assert_eq!(res.status, StatusCode::OK);
}

// ---- OpenAPI and logging --------------------------------------------------

#[test]
fn the_cookie_scheme_is_documented() {
    let app = app(layer(MemoryStore::default(), SessionConfig::new()));
    let doc = app.openapi().unwrap().to_value();
    let scheme = &doc["components"]["securitySchemes"]["sessionCookie"];
    assert_eq!(scheme["type"], "apiKey");
    assert_eq!(scheme["in"], "cookie");
    assert_eq!(scheme["name"], "__Host-session");
    let security = &doc["paths"]["/me"]["get"]["security"];
    assert!(
        security
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.get("sessionCookie").is_some())
    );
    assert!(
        security
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_object().unwrap().is_empty())
    );
}

#[test]
fn debug_output_never_shows_ids_or_data() {
    let record = siderite_sessions::SessionRecord {
        user: Some("ada@example.com".into()),
        data: serde_json::Map::from_iter([("k".into(), "TOPSECRET".into())]),
        ..Default::default()
    };
    let text = format!("{record:?}");
    assert!(
        !text.contains("TOPSECRET") && !text.contains("ada@"),
        "{text}"
    );
}
