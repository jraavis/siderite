//! Server-sent events: [`Event`] encodes one event of the
//! `text/event-stream` format, and [`Sse`] streams events as a response.
//!
//! - Data may hold any text: each line becomes its own `data:` line
//!   (`\n`, `\r\n` and `\r` all end a line), so a client receives the
//!   text back with `\n` line ends.
//! - Event names and ids are single lines: a CR or LF in them (or a NUL in
//!   an id, which clients ignore) is rejected with [`SseError`] instead of
//!   splitting the event.
//! - Responses carry `Content-Type: text/event-stream` and
//!   `Cache-Control: no-store`, so no cache stores or replays a stream.
//! - An `Err` from the event stream ends the response body abruptly; the
//!   error itself is never sent (clients see the connection end and may
//!   reconnect). Send an event first if the client should know why.

use crate::response::{IntoResponse, Response, SUCCESS};
use crate::responses::StreamingResponse;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use http::HeaderValue;
use http::header::CACHE_CONTROL;
use serde::Serialize;
use siderite_openapi::{Operation, SchemaObject, SchemaRegistry};
use std::error::Error as StdError;
use std::fmt::Write;
use std::time::Duration;
use thiserror::Error;

/// The media type of an event stream.
pub const EVENT_STREAM: &str = "text/event-stream";

/// A field value that would break the event framing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SseError {
    /// An event name containing a line break.
    #[error("SSE event names must not contain CR or LF")]
    EventName,
    /// An id containing a line break or NUL.
    #[error("SSE ids must not contain CR, LF or NUL")]
    Id,
}

/// One server-sent event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Event {
    comment: Option<String>,
    event: Option<String>,
    id: Option<String>,
    retry: Option<Duration>,
    data: Option<String>,
}

impl Event {
    /// An event carrying `text` (any text; line breaks are kept).
    pub fn data(text: impl Into<String>) -> Self {
        Self {
            data: Some(text.into()),
            ..Self::default()
        }
    }

    /// An event carrying `value` as JSON.
    ///
    /// # Errors
    /// Serialization errors of `value`.
    pub fn json<T: Serialize>(value: &T) -> Result<Self, serde_json::Error> {
        Ok(Self::data(serde_json::to_string(value)?))
    }

    /// A comment only (`: text`), which clients ignore: useful to keep a
    /// connection active.
    pub fn comment(text: impl Into<String>) -> Self {
        Self {
            comment: Some(text.into()),
            ..Self::default()
        }
    }

    /// Name the event (the client's `addEventListener` type).
    ///
    /// # Errors
    /// [`SseError::EventName`] when `name` contains CR or LF.
    pub fn event(mut self, name: impl Into<String>) -> Result<Self, SseError> {
        let name = name.into();
        if name.contains(['\r', '\n']) {
            return Err(SseError::EventName);
        }
        self.event = Some(name);
        Ok(self)
    }

    /// Set the id clients send back as `Last-Event-ID` when reconnecting.
    ///
    /// # Errors
    /// [`SseError::Id`] when `id` contains CR, LF or NUL.
    pub fn id(mut self, id: impl Into<String>) -> Result<Self, SseError> {
        let id = id.into();
        if id.contains(['\r', '\n', '\0']) {
            return Err(SseError::Id);
        }
        self.id = Some(id);
        Ok(self)
    }

    /// Ask clients to wait `delay` before reconnecting (whole
    /// milliseconds).
    #[must_use]
    pub fn retry(mut self, delay: Duration) -> Self {
        self.retry = Some(delay);
        self
    }

    /// The event in `text/event-stream` form, ending with a blank line.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        if let Some(comment) = &self.comment {
            for line in lines(comment) {
                let _ = writeln!(out, ": {line}");
            }
        }
        if let Some(event) = &self.event {
            let _ = writeln!(out, "event: {event}");
        }
        if let Some(id) = &self.id {
            let _ = writeln!(out, "id: {id}");
        }
        if let Some(retry) = self.retry {
            let _ = writeln!(out, "retry: {}", retry.as_millis());
        }
        if let Some(data) = &self.data {
            for line in lines(data) {
                let _ = writeln!(out, "data: {line}");
            }
        }
        out.push('\n');
        out
    }
}

/// The lines of `text`, split at `\r\n`, `\r` or `\n`; an empty text is
/// one empty line.
fn lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        match rest.find(['\r', '\n']) {
            Some(at) => {
                out.push(&rest[..at]);
                let skip = if rest[at..].starts_with("\r\n") { 2 } else { 1 };
                rest = &rest[at + skip..];
            }
            None => {
                out.push(rest);
                return out;
            }
        }
    }
}

/// A `text/event-stream` response from a stream of events.
pub struct Sse<S> {
    stream: S,
}

impl<S, E> Sse<S>
where
    S: Stream<Item = Result<Event, E>> + Send + 'static,
    E: Into<Box<dyn StdError + Send + Sync>> + Send + 'static,
{
    /// Stream `events`; an `Err` ends the response (see the module docs).
    pub fn new(events: S) -> Self {
        Self { stream: events }
    }
}

impl<S, E> IntoResponse for Sse<S>
where
    S: Stream<Item = Result<Event, E>> + Send + 'static,
    E: Into<Box<dyn StdError + Send + Sync>> + Send + 'static,
{
    fn into_response(self) -> Response {
        let bytes = self
            .stream
            .map(|item| item.map(|event| Bytes::from(event.encode())));
        let stream = StreamingResponse::new(bytes).content_type(EVENT_STREAM);
        let mut response = stream.into_response();
        let no_store = HeaderValue::from_static("no-store");
        response.headers_mut().insert(CACHE_CONTROL, no_store);
        response
    }

    fn describe(op: &mut Operation, _registry: &mut SchemaRegistry) {
        let schema = SchemaObject::of_type("string");
        let content = Some((EVENT_STREAM, schema));
        op.add_response(SUCCESS, "Server-sent events", content);
    }
}

#[cfg(test)]
mod tests;
