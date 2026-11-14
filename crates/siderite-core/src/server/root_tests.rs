//! The production serving path (`App::run`) hands each request the peer
//! address and the server-wide values without engine router layers.

use super::*;
use crate::{RawRequest, get};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn peer_and_phase(ready: Readiness, raw: RawRequest) -> String {
    let peer = raw
        .0
        .extensions()
        .get::<ConnectInfo<Peer>>()
        .map(|info| info.0.ip());
    format!("peer={peer:?} phase={:?}", ready.phase())
}

#[tokio::test]
async fn served_requests_carry_peer_address_and_readiness() -> TestResult {
    let app = App::new().route("/", get(peer_and_phase));
    let limits = app.server_limits;
    let budget = app.shutdown_budget;
    let (root, lifespan) = app.build_root()?;
    let server = lifespan.server.clone().ok_or("no server owner")?;
    let mut owner = lifespan.supervise()?;
    owner.ready().await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (stop, signal) = oneshot::channel::<()>();
    let shutdown = async move {
        let _ = signal.await;
    };
    let task = tokio::spawn(serve(listener, root, limits, server, budget, shutdown));
    let mut socket = TcpStream::connect(addr).await?;
    socket
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await?;
    let mut response = String::new();
    socket.read_to_string(&mut response).await?;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains("peer=Some(127.0.0.1) phase=Ready"),
        "{response}"
    );
    let _ = stop.send(());
    let (result, deadline) = task.await?;
    result?;
    owner.shutdown_at(deadline).await?;
    Ok(())
}
