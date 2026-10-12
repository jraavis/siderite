//! Bounds on what one client can make the server do: message size on the
//! way in, time and result size per tool call.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, ReadBuf};

/// The server's bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    /// Largest JSON-RPC message accepted, in bytes (one line on stdio).
    pub max_message: usize,
    /// Longest a tool call may run.
    pub call_timeout: Duration,
    /// Largest tool result returned, in bytes of JSON.
    pub max_result: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message: 1 << 20,
            call_timeout: Duration::from_secs(30),
            max_result: 256 << 10,
        }
    }
}

/// A reader that fails once a line grows past `max` bytes, so one
/// oversized message cannot make the server buffer without limit.
#[derive(Debug)]
pub(crate) struct LineLimit<R> {
    inner: R,
    max: usize,
    line: usize,
    tripped: Arc<AtomicBool>,
}

impl<R> LineLimit<R> {
    /// Bound `inner`'s lines to `max` bytes.
    pub(crate) fn new(inner: R, max: usize) -> Self {
        Self {
            inner,
            max,
            line: 0,
            tripped: Arc::default(),
        }
    }

    /// Becomes `true` once a line broke the limit (the reader then fails,
    /// which a transport may report as an ordinary close).
    pub(crate) fn tripped(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.tripped)
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for LineLimit<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = polled {
            let fresh = &buf.filled()[before..];
            let mut line = self.line;
            for byte in fresh {
                line = if *byte == b'\n' { 0 } else { line + 1 };
                if line > self.max {
                    self.tripped.store(true, Ordering::Relaxed);
                    let msg = format!("a message exceeds {} bytes", self.max);
                    let kind = io::ErrorKind::InvalidData;
                    return Poll::Ready(Err(io::Error::new(kind, msg)));
                }
            }
            self.line = line;
        }
        polled
    }
}
