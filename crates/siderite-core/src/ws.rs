//! WebSocket upgrade and connection handling.

use crate::body::Body;
use crate::error::ApiError;
use crate::extract::FromRequestParts;
use crate::response::{IntoResponse, Response};
use bytes::Bytes;
use http::request::Parts;
use siderite_openapi::{Operation, SchemaRegistry};
use std::future::Future;
use thiserror::Error;

/// Extractor that upgrades an HTTP request to a WebSocket.
///
/// Failed upgrades (missing headers, wrong method, non-upgradable
/// connection) become `400` or `426` problem details responses.
pub struct WebSocketUpgrade {
    inner: axum::extract::ws::WebSocketUpgrade,
    owner: crate::server::tasks::TaskHandle,
    permit: tokio::sync::OwnedSemaphorePermit,
}

impl std::fmt::Debug for WebSocketUpgrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketUpgrade").finish_non_exhaustive()
    }
}

impl FromRequestParts for WebSocketUpgrade {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        use axum::extract::FromRequestParts as _;
        match axum::extract::ws::WebSocketUpgrade::from_request_parts(parts, &()).await {
            Ok(inner) => {
                let owner = parts
                    .extensions
                    .get::<crate::service::ServerValues>()
                    .map(|values| values.upgrades.clone())
                    .or_else(|| {
                        parts
                            .extensions
                            .get::<crate::server::tasks::TaskHandle>()
                            .cloned()
                    })
                    .ok_or_else(upgrade_unavailable)?;
                let permit = owner.reserve().ok_or_else(upgrade_unavailable)?;
                Ok(Self {
                    inner,
                    owner,
                    permit,
                })
            }
            Err(rejection) => Err(ApiError::new(rejection.status(), rejection.body_text())),
        }
    }
}

impl WebSocketUpgrade {
    /// Limit incoming message size in bytes (axum default is 64 MiB).
    #[must_use]
    pub fn max_message_size(self, max: usize) -> Self {
        Self {
            inner: self.inner.max_message_size(max),
            owner: self.owner,
            permit: self.permit,
        }
    }

    /// Complete the upgrade and run `callback` on the established socket.
    ///
    /// The returned [`WebSocketResponse`] must be returned from the handler.
    #[must_use]
    pub fn on_upgrade<F, Fut>(self, callback: F) -> WebSocketResponse
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Self {
            inner,
            owner,
            permit,
        } = self;
        let (established, socket) = tokio::sync::oneshot::channel();
        if !owner.spawn(permit, async move {
            if let Ok(socket) = socket.await {
                callback(WebSocket { inner: socket }).await;
            }
        }) {
            let response = upgrade_unavailable().into_response();
            return WebSocketResponse(response.map(Body::into_inner));
        }
        let response = inner.on_upgrade(move |socket| async move {
            // Axum's handoff owns only the channel. Pending callbacks and
            // established sockets remain under the lifespan supervisor.
            let _ = established.send(socket);
        });
        WebSocketResponse(response)
    }
}

fn upgrade_unavailable() -> ApiError {
    ApiError::new(
        http::StatusCode::SERVICE_UNAVAILABLE,
        "WebSocket admission is unavailable.",
    )
    .with_header(
        http::header::RETRY_AFTER,
        http::HeaderValue::from_static("1"),
    )
}

/// Response produced by [`WebSocketUpgrade::on_upgrade`].
pub struct WebSocketResponse(axum::response::Response);

impl IntoResponse for WebSocketResponse {
    fn into_response(self) -> Response {
        self.0.map(Body::from_inner)
    }

    fn describe(op: &mut Operation, _registry: &mut SchemaRegistry) {
        op.add_response("101", "Switching Protocols", None);
    }
}

/// An established WebSocket connection.
pub struct WebSocket {
    inner: axum::extract::ws::WebSocket,
}

impl std::fmt::Debug for WebSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocket").finish_non_exhaustive()
    }
}

impl WebSocket {
    /// Receive the next message, or `None` if the connection has closed.
    pub async fn recv(&mut self) -> Option<Result<Message, WsError>> {
        self.inner.recv().await.map(|result| {
            result
                .map(Message::from_engine)
                .map_err(WsError::from_engine)
        })
    }

    /// Send a message.
    ///
    /// # Errors
    /// Returns [`WsError`] if the connection is closed or the send fails.
    pub async fn send(&mut self, message: Message) -> Result<(), WsError> {
        self.inner
            .send(message.into_engine())
            .await
            .map_err(WsError::from_engine)
    }

    /// Send a close frame with `code` and `reason`.
    ///
    /// # Errors
    /// Returns [`WsError`] if the connection is closed or the send fails.
    pub async fn close(&mut self, code: u16, reason: impl Into<String>) -> Result<(), WsError> {
        self.send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await
    }
}

/// A WebSocket message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// UTF-8 text.
    Text(String),
    /// Binary payload.
    Binary(Bytes),
    /// Ping (payload at most 125 bytes).
    Ping(Bytes),
    /// Pong (payload at most 125 bytes).
    Pong(Bytes),
    /// Close frame.
    Close(Option<CloseFrame>),
}

/// Payload of a WebSocket close frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseFrame {
    /// Close status code.
    pub code: u16,
    /// Close reason.
    pub reason: String,
}

impl Message {
    fn from_engine(message: axum::extract::ws::Message) -> Self {
        match message {
            axum::extract::ws::Message::Text(text) => Self::Text(text.to_string()),
            axum::extract::ws::Message::Binary(data) => Self::Binary(data),
            axum::extract::ws::Message::Ping(data) => Self::Ping(data),
            axum::extract::ws::Message::Pong(data) => Self::Pong(data),
            axum::extract::ws::Message::Close(frame) => {
                Self::Close(frame.map(|frame| CloseFrame {
                    code: frame.code,
                    reason: frame.reason.to_string(),
                }))
            }
        }
    }

    fn into_engine(self) -> axum::extract::ws::Message {
        match self {
            Self::Text(text) => axum::extract::ws::Message::Text(text.into()),
            Self::Binary(data) => axum::extract::ws::Message::Binary(data),
            Self::Ping(data) => axum::extract::ws::Message::Ping(data),
            Self::Pong(data) => axum::extract::ws::Message::Pong(data),
            Self::Close(frame) => axum::extract::ws::Message::Close(frame.map(|frame| {
                axum::extract::ws::CloseFrame {
                    code: frame.code,
                    reason: frame.reason.into(),
                }
            })),
        }
    }
}

/// Error sending or receiving a WebSocket message.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct WsError(String);

impl WsError {
    fn from_engine(err: axum::Error) -> Self {
        Self(err.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::routing::get;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[test]
    fn message_conversions_round_trip() {
        let messages = [
            Message::Text("hello".into()),
            Message::Binary(Bytes::from_static(b"bin")),
            Message::Ping(Bytes::from_static(b"ping")),
            Message::Pong(Bytes::from_static(b"pong")),
            Message::Close(None),
            Message::Close(Some(CloseFrame {
                code: 1000,
                reason: "bye".into(),
            })),
        ];
        for message in messages {
            let back = Message::from_engine(message.clone().into_engine());
            assert_eq!(message, back);
        }
    }

    async fn echo(ws: WebSocketUpgrade) -> WebSocketResponse {
        ws.on_upgrade(|mut socket| async move {
            while let Some(Ok(message)) = socket.recv().await {
                if socket.send(message).await.is_err() {
                    break;
                }
            }
        })
    }

    #[tokio::test]
    async fn websocket_http11_upgrade_round_trip() {
        let app = App::new().route("/ws", get(echo));
        let (router, _hooks) = app.build().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        let mut stream = connect_with_retry(addr).await;
        stream
            .write_all(
                b"GET /ws HTTP/1.1\r\n\
Host: localhost\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
Sec-WebSocket-Version: 13\r\n\
\r\n",
            )
            .await
            .unwrap();

        let headers = read_http_headers(&mut stream).await;
        assert!(
            headers.starts_with("HTTP/1.1 101"),
            "expected 101 Switching Protocols, got: {headers}"
        );
        assert!(headers.to_ascii_lowercase().contains("upgrade: websocket"));

        // Masked text frame "hi".
        stream
            .write_all(&[0x81, 0x82, 0x01, 0x02, 0x03, 0x04, b'h' ^ 0x01, b'i' ^ 0x02])
            .await
            .unwrap();

        let mut buf = [0_u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, [0x81, 0x02, b'h', b'i']);
    }

    async fn connect_with_retry(addr: std::net::SocketAddr) -> TcpStream {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            match TcpStream::connect(addr).await {
                Ok(stream) => return stream,
                Err(err) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let _ = err;
                }
                Err(err) => panic!("connect: {err}"),
            }
        }
    }

    async fn read_http_headers(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut byte = [0_u8; 1];
        loop {
            stream.read_exact(&mut byte).await.unwrap();
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
            if buf.len() > 8192 {
                panic!("HTTP response headers too large");
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }
}
