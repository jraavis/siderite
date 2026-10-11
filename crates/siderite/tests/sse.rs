//! Server-sent events through a route.
#![allow(clippy::unwrap_used)]

use siderite::prelude::*;
use siderite::sse::{Event, Sse};
use siderite_testkit::TestClient;

type Item = Result<Event, std::convert::Infallible>;
type Events = futures_util::stream::Iter<std::vec::IntoIter<Item>>;

/// Three ticks.
#[get("/ticks")]
async fn ticks() -> Sse<Events> {
    let events = (1..=3).map(|n| {
        let event = Event::data(format!("tick {n}")).event("tick").unwrap();
        Ok(event.id(n.to_string()).unwrap())
    });
    Sse::new(futures_util::stream::iter(events.collect::<Vec<_>>()))
}

#[tokio::test]
async fn a_route_streams_events() {
    let app = App::new().routes(routes![ticks]);
    let document = serde_json::to_value(app.openapi().unwrap()).unwrap();
    let ok = &document["paths"]["/ticks"]["get"]["responses"]["200"];
    assert!(ok["content"]["text/event-stream"].is_object(), "{ok}");
    let client = TestClient::new(app);
    let response = client.get("/ticks").await.unwrap();
    assert_eq!(response.status, 200);
    let kind = response.headers.get("content-type").unwrap();
    assert_eq!(kind, "text/event-stream");
    let text = response.text();
    assert!(
        text.starts_with("event: tick\nid: 1\ndata: tick 1\n\n"),
        "{text}"
    );
    assert_eq!(text.matches("event: tick").count(), 3);
}
