//! The session cookie and ids never reach logs.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use siderite_sessions::SessionConfig;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
    next_id: Arc<AtomicUsize>,
}

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(
        &mut self,
        field: &tracing::field::Field,
        value: &dyn std::fmt::Debug,
    ) {
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
    ) -> tracing::span::Id {
        let mut fields = Fields(format!("span {} ", attrs.metadata().name()));
        attrs.record(&mut fields);
        self.lines.lock().unwrap().push(fields.0);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as u64 + 1;
        tracing::span::Id::from_u64(id)
    }

    fn record(
        &self,
        _: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
    ) {
        let mut fields = Fields("record ".to_owned());
        values.record(&mut fields);
        self.lines.lock().unwrap().push(fields.0);
    }

    fn record_follows_from(
        &self,
        _: &tracing::span::Id,
        _: &tracing::span::Id,
    ) {
    }

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields("event ".to_owned());
        event.record(&mut fields);
        self.lines.lock().unwrap().push(fields.0);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn cookie_ids_and_hashes_stay_out_of_logs() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let spy = Spy::new();
    let layer = layer(spy.clone(), SessionConfig::new());
    let app = app(layer).request_id().request_logging();
    let client = siderite_testkit::TestClient::new(app);

    let cookie = pair(&post_as(&client, "/login", None).await).unwrap();
    get_as(&client, "/me", Some(&cookie)).await;
    // Also through the failure paths, which log a warning.
    Spy::flag(&spy.fail_load, true);
    let res = get_as(&client, "/me", Some(&cookie)).await;
    assert_eq!(res.status, 503);
    Spy::flag(&spy.fail_load, false);
    Spy::flag(&spy.fail_save, true);
    post_as(&client, "/set", Some(&cookie)).await;

    let (_, raw) = cookie.split_once('=').unwrap();
    let hash = hash_of(&cookie);
    let logged = capture.lines.lock().unwrap().join("\n");
    assert!(
        logged.contains("http.request"),
        "logging was active:\n{logged}"
    );
    assert!(
        logged.contains("session store"),
        "warnings fired:\n{logged}"
    );
    for secret in [raw, hash.as_str(), "__Host-session="] {
        assert!(!logged.contains(secret), "{secret} leaked:\n{logged}");
    }
}
