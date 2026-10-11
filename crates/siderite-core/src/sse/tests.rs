//! Event encoding, validation and the response.

use super::*;
use crate::response::IntoResponse;
use http::header::CONTENT_TYPE;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn data_lines_are_split_on_every_line_break() {
    let event = Event::data("one\ntwo\r\nthree\rfour");
    let want = "data: one\ndata: two\ndata: three\ndata: four\n\n";
    assert_eq!(event.encode(), want);
    assert_eq!(Event::data("").encode(), "data: \n\n");
    assert_eq!(Event::data("end\n").encode(), "data: end\ndata: \n\n");
}

#[test]
fn fields_are_encoded_in_order() -> TestResult {
    let event = Event::data("hi")
        .event("greeting")?
        .id("42")?
        .retry(Duration::from_millis(1500));
    let want = "event: greeting\nid: 42\nretry: 1500\ndata: hi\n\n";
    assert_eq!(event.encode(), want);
    let comment = Event::comment("keep\nalive");
    assert_eq!(comment.encode(), ": keep\n: alive\n\n");
    let json = Event::json(&serde_json::json!({"n": 1, "s": "a\nb"}))?;
    assert_eq!(json.encode(), "data: {\"n\":1,\"s\":\"a\\nb\"}\n\n");
    Ok(())
}

#[test]
fn names_and_ids_cannot_break_framing() {
    let event = || Event::data("x");
    assert_eq!(event().event("a\nb"), Err(SseError::EventName));
    assert_eq!(event().event("a\rb"), Err(SseError::EventName));
    assert_eq!(event().id("1\n2"), Err(SseError::Id));
    assert_eq!(event().id("1\0"), Err(SseError::Id));
    assert!(event().event("ok name").is_ok());
    assert!(event().id("").is_ok());
}

fn events(
    items: Vec<Result<Event, std::io::Error>>,
) -> Sse<impl Stream<Item = Result<Event, std::io::Error>> + Send + 'static> {
    Sse::new(futures_util::stream::iter(items))
}

#[tokio::test]
async fn the_response_streams_events_without_caching() -> TestResult {
    let response = events(vec![
        Ok(Event::data("a").id("1")?),
        Ok(Event::data("b").id("2")?),
    ])
    .into_response();
    let headers = response.headers();
    assert_eq!(headers.get(CONTENT_TYPE).ok_or("type")?, EVENT_STREAM);
    assert_eq!(headers.get(CACHE_CONTROL).ok_or("cache")?, "no-store");
    let body = response.into_body().into_bytes().await?;
    let text = String::from_utf8(body)?;
    assert_eq!(text, "id: 1\ndata: a\n\nid: 2\ndata: b\n\n");
    Ok(())
}

#[tokio::test]
async fn a_stream_error_ends_the_body_without_its_message() {
    let failing = events(vec![
        Ok(Event::data("first")),
        Err(std::io::Error::other("secret detail")),
        Ok(Event::data("never")),
    ]);
    let body = failing.into_response().into_body();
    let read = body.into_bytes().await;
    assert!(read.is_err(), "the body ends with an error, not cleanly");
}

#[test]
fn the_document_names_the_media_type() {
    let mut op = Operation::default();
    let mut registry = SchemaRegistry::default();
    type Item = Result<Event, std::io::Error>;
    type Events = Sse<futures_util::stream::Empty<Item>>;
    Events::describe(&mut op, &mut registry);
    let text = serde_json::to_string(&op).unwrap_or_default();
    assert!(text.contains(EVENT_STREAM), "{text}");
}
