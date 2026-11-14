//! Owned HTTP connection and HTTP/2 stream tasks.

mod readiness;
pub(crate) mod tasks;
pub use readiness::{Readiness, ServerPhase};
#[cfg(test)]
mod root_tests;
#[cfg(all(test, unix))]
mod signal_tests;
#[cfg(test)]
mod tests;

use crate::service::RootRouter;
use crate::{App, ServerError};
use axum::extract::ConnectInfo;
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr as Peer;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::Instant;
use tower::Service;

/// Hard transport admission bounds applied by [`App::run`].
#[derive(Debug, Clone, Copy)]
pub struct ServerLimits {
    /// Maximum accepted sockets, including idle keep-alive connections.
    pub max_connections: usize,
    /// Concurrent HTTP/2 streams permitted on each connection.
    pub max_http2_streams: u32,
    /// Active or pending WebSocket upgrades across the app tree.
    pub max_websockets: usize,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            max_connections: 1024,
            max_http2_streams: 100,
            max_websockets: 128,
        }
    }
}

impl App {
    /// Configure bounded transport admission before binding the server.
    ///
    /// Args:
    ///     limits: Accepted socket and per-connection HTTP/2 limits.
    ///
    /// Returns:
    ///     This application; invalid bounds become configuration errors.
    #[must_use]
    pub fn server_limits(mut self, limits: ServerLimits) -> Self {
        if limits.capacity().is_none() {
            self.config_errors.push("invalid server limits".to_owned());
        } else {
            self.server_limits = limits;
        }
        self
    }
}

impl ServerLimits {
    fn capacity(self) -> Option<usize> {
        let streams = usize::try_from(self.max_http2_streams).ok()?;
        let tasks = self.max_connections.checked_mul(streams.checked_add(4)?)?;
        (self.max_connections > 0
            && streams > 0
            && tasks <= 1_000_000
            && self.max_websockets <= 1_000_000)
            .then_some(tasks)
    }
}

#[derive(Clone)]
pub(crate) struct ServerOwners {
    connections: tasks::TaskOwner,
    streams: tasks::TaskOwner,
    pub(crate) readiness: Readiness,
}

impl ServerOwners {
    pub(crate) fn new(limits: ServerLimits) -> Self {
        Self {
            connections: tasks::TaskOwner::new(limits.max_connections),
            streams: tasks::TaskOwner::new(limits.capacity().unwrap_or(1)),
            readiness: Readiness::new(),
        }
    }

    pub(crate) async fn stop_at(&self, deadline: Instant) -> bool {
        self.readiness.draining();
        let connections = self.connections.stop_at(deadline).await;
        let streams = self.streams.stop_at(deadline).await;
        connections && streams
    }
}

struct AbortOnDrop(ServerOwners);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.readiness.draining();
        self.0.connections.abort();
        self.0.streams.abort();
    }
}

/// One accepted connection's service: the root router plus the peer
/// address, inserted as axum's `ConnectInfo`.
#[derive(Clone)]
struct Connection {
    root: RootRouter,
    peer: ConnectInfo<Peer>,
}

impl<B> Service<http::Request<B>> for Connection
where
    B: http_body::Body<Data = bytes::Bytes> + Send + 'static,
    B::Error: Into<axum::BoxError>,
{
    type Response = axum::response::Response;
    type Error = Infallible;
    type Future = axum::routing::future::RouteFuture<Infallible>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.root.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        let mut req = req.map(axum::body::Body::new);
        req.extensions_mut().insert(self.peer);
        self.root.call(req)
    }
}

pub(crate) async fn serve<F>(
    listener: TcpListener,
    router: impl Into<RootRouter>,
    limits: ServerLimits,
    owners: ServerOwners,
    budget: Duration,
    shutdown: F,
) -> (Result<(), ServerError>, Instant)
where
    F: Future<Output = ()> + Send,
{
    let _guard = AbortOnDrop(owners.clone());
    let streams = &owners.streams;
    let (draining, receiver) = watch::channel(false);
    let workers = &owners.connections;
    let root: RootRouter = router.into();
    tokio::pin!(shutdown);
    let error = loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break None,
            joined = async {
                match workers.join_next().await {
                    Some(result) => Some(result),
                    None => std::future::pending().await,
                }
            } => {
                if let Some(Err(error)) = joined {
                    tracing::error!(%error, "HTTP connection task failed");
                }
            }
            accepted = async {
                let permit = workers.handle().acquire().await;
                let accepted = listener.accept().await;
                (permit, accepted)
            } => {
                let (permit, accepted) = accepted;
                let (socket, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Some(ServerError::Serve(error)),
                };
                let Some(permit) = permit else { break None; };
                let per_connection = Connection {
                    root: root.clone(),
                    peer: ConnectInfo(peer),
                };
                let executor = streams.handle();
                let mut stopping = receiver.clone();
                workers.handle().spawn(permit, async move {
                    let mut builder = Builder::new(executor);
                    builder.http2()
                        .max_concurrent_streams(limits.max_http2_streams);
                    let io = TokioIo::new(socket);
                    let service = TowerToHyperService::new(per_connection);
                    let connection = builder
                        .serve_connection_with_upgrades(io, service);
                    tokio::pin!(connection);
                    let result = tokio::select! {
                        result = &mut connection => result,
                        _ = stopping.changed() => {
                            connection.as_mut().graceful_shutdown();
                            connection.await
                        }
                    };
                    if let Err(error) = result {
                        tracing::debug!(%error, "HTTP connection ended");
                    }
                });
            }
        }
    };
    drop(listener);
    owners.readiness.draining();
    let deadline = Instant::now() + budget;
    let _ = draining.send(true);
    let timed_out = !owners.stop_at(deadline).await;
    let result = error.map_or_else(
        || {
            if timed_out {
                Err(ServerError::ShutdownTimeout)
            } else {
                Ok(())
            }
        },
        Err,
    );
    (result, deadline)
}
