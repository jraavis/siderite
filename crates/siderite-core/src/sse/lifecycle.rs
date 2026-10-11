//! The byte stream behind [`Sse`](super::Sse): encoded events, idle
//! heartbeats and an optional end signal.

use super::Event;
use bytes::Bytes;
use futures_util::Stream;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::{Instant, Sleep};

/// A future that ends the stream when it resolves.
pub(crate) type Until = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The heartbeat interval, if any.
type Every = Option<Duration>;
type Next<E> = Poll<Option<Result<Bytes, E>>>;

/// The comment sent when no event was sent for the heartbeat interval.
const HEARTBEAT: &[u8] = b":\n\n";

pub(crate) struct Lifecycle<S> {
    events: Pin<Box<S>>,
    heartbeat: Option<(Duration, Pin<Box<Sleep>>)>,
    until: Option<Until>,
    done: bool,
}

impl<S> Lifecycle<S> {
    pub(crate) fn new(events: S, every: Every, until: Option<Until>) -> Self {
        let timer = |every| (every, Box::pin(tokio::time::sleep(every)));
        let heartbeat = every.map(timer);
        Self {
            events: Box::pin(events),
            heartbeat,
            until,
            done: false,
        }
    }

    /// Restart the idle timer after anything was sent.
    fn sent(&mut self) {
        if let Some((every, sleep)) = &mut self.heartbeat {
            sleep.as_mut().reset(Instant::now() + *every);
        }
    }
}

impl<S, E> Stream for Lifecycle<S>
where
    S: Stream<Item = Result<Event, E>>,
{
    type Item = Result<Bytes, E>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Next<E> {
        // Every field is boxed or `Unpin`, so the wrapper is `Unpin`.
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if let Some(until) = &mut this.until
            && until.as_mut().poll(cx).is_ready()
        {
            // Dropping the events ends their work at once.
            this.done = true;
            return Poll::Ready(None);
        }
        match this.events.as_mut().poll_next(cx) {
            Poll::Ready(Some(item)) => {
                this.sent();
                Poll::Ready(Some(item.map(|e| Bytes::from(e.encode()))))
            }
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => {
                let due = |(_, sleep): &mut (Duration, Pin<Box<Sleep>>)| {
                    sleep.as_mut().poll(cx).is_ready()
                };
                let idle = this.heartbeat.as_mut().is_some_and(due);
                if !idle {
                    return Poll::Pending;
                }
                this.sent();
                Poll::Ready(Some(Ok(Bytes::from_static(HEARTBEAT))))
            }
        }
    }
}
