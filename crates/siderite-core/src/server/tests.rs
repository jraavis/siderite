//! Real socket ownership at graceful, forced and cancelled shutdown.

use super::*;
use crate::{State, StreamingResponse, WebSocketUpgrade, get};
use bytes::Bytes;
use hyper::client::conn::http2::handshake as h2_handshake;
use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Notify, oneshot};

type Failure = Box<dyn std::error::Error>;
type TestResult = Result<(), Failure>;
type SocketResult = Result<TcpStream, Failure>;
type ServerTask = tokio::task::JoinHandle<Result<(), ServerError>>;

#[derive(Clone, Default)]
struct Probe {
    started: Arc<Notify>,
    cleaned: Arc<Notify>,
    drops: Arc<AtomicUsize>,
    clean_order: Arc<AtomicBool>,
}

struct Marker(Probe);

impl Drop for Marker {
    fn drop(&mut self) {
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}

struct Running {
    task: ServerTask,
    stop: oneshot::Sender<()>,
    addr: Peer,
    readiness: Readiness,
}

async fn start(app: App, probe: &Probe) -> Result<Running, ServerError> {
    let cleanup = probe.clone();
    let app = app
        .with_state(probe.clone())
        .shutdown_timeout(Duration::from_millis(100))
        .on_shutdown(move || async move {
            let dropped = cleanup.drops.load(Ordering::SeqCst);
            cleanup.clean_order.store(dropped == 1, Ordering::SeqCst);
            cleanup.cleaned.notify_one();
            Ok(())
        });
    let limits = app.server_limits;
    let budget = app.shutdown_budget;
    let (router, lifespan) = app.build()?;
    let server = lifespan
        .server
        .clone()
        .ok_or_else(|| ServerError::Configuration("owner".to_owned()))?;
    let readiness = server.readiness.clone();
    let mut owner = lifespan.supervise()?;
    owner.ready().await?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(ServerError::Serve)?;
    let addr = listener.local_addr().map_err(ServerError::Serve)?;
    let (stop, signal) = oneshot::channel();
    let task = tokio::spawn(async move {
        let shutdown = async move {
            let _ = signal.await;
        };
        let work = serve(listener, router, limits, server, budget, shutdown);
        let (result, deadline) = work.await;
        let stopped = owner.shutdown_at(deadline).await;
        result?;
        stopped
    });
    Ok(Running {
        task,
        stop,
        addr,
        readiness,
    })
}

async fn blocked(State(probe): State<Probe>) -> &'static str {
    let _marker = Marker((*probe).clone());
    probe.started.notify_one();
    pending().await
}

async fn complete(State(probe): State<Probe>) -> &'static str {
    let _marker = Marker((*probe).clone());
    probe.started.notify_one();
    "complete"
}

async fn streaming(State(probe): State<Probe>) -> StreamingResponse {
    let marker = Marker((*probe).clone());
    let stream = futures_util::stream::once(async move {
        let _marker = marker;
        probe.started.notify_one();
        pending::<Result<Bytes, std::io::Error>>().await
    });
    StreamingResponse::new(stream)
}

async fn upgraded(
    upgrade: WebSocketUpgrade,
    State(probe): State<Probe>,
) -> crate::WebSocketResponse {
    upgrade.on_upgrade(move |socket| async move {
        let _socket = socket;
        let _marker = Marker((*probe).clone());
        probe.started.notify_one();
        pending::<()>().await;
    })
}

async fn request(addr: Peer, websocket: bool) -> SocketResult {
    let mut socket = TcpStream::connect(addr).await?;
    let request = if websocket {
        "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
         Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    } else {
        "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n"
    };
    socket.write_all(request.as_bytes()).await?;
    Ok(socket)
}

async fn forced(app: App, websocket: bool) -> TestResult {
    let probe = Probe::default();
    let run = start(app, &probe).await?;
    let mut socket = request(run.addr, websocket).await?;
    reached(&probe.started).await?;
    let _ = run.stop.send(());
    let result = tokio::time::timeout(Duration::from_secs(2), run.task).await?;
    assert!(matches!(result?, Err(ServerError::ShutdownTimeout)));
    assert_eq!(run.readiness.phase(), ServerPhase::Stopped);
    assert!(probe.clean_order.load(Ordering::SeqCst));
    let mut buffer = Vec::new();
    let reading = socket.read_to_end(&mut buffer);
    tokio::time::timeout(Duration::from_secs(2), reading).await??;
    Ok(())
}

#[tokio::test]
async fn readiness_tracks_startup_and_resource_teardown() -> TestResult {
    let (started, teardown) = oneshot::channel();
    let (finish, resume) = oneshot::channel();
    let app = App::new().on_shutdown(move || async move {
        let _ = started.send(());
        let _ = resume.await;
        Ok(())
    });
    let (_, mut lifespan) = app.build()?;
    let readiness = lifespan
        .server
        .as_ref()
        .ok_or("missing server")?
        .readiness
        .clone();
    assert_eq!(readiness.phase(), ServerPhase::Starting);
    lifespan.startup().await?;
    assert!(readiness.is_ready());
    let mut shutdown = Box::pin(lifespan.shutdown());
    tokio::select! {
        result = teardown => result?,
        result = &mut shutdown => panic!("unexpected cleanup: {result:?}"),
    }
    assert_eq!(readiness.phase(), ServerPhase::Draining);
    let _ = finish.send(());
    shutdown.await?;
    assert_eq!(readiness.phase(), ServerPhase::Stopped);
    assert!(!readiness.is_ready());
    Ok(())
}

#[tokio::test]
async fn failed_startup_never_advertises_readiness() -> TestResult {
    let app = App::new().on_startup(reject_startup);
    let (_, mut lifespan) = app.build()?;
    let readiness = lifespan
        .server
        .as_ref()
        .ok_or("missing server")?
        .readiness
        .clone();
    assert!(lifespan.startup().await.is_err());
    assert_eq!(readiness.phase(), ServerPhase::Stopped);
    assert!(!readiness.is_ready());
    Ok(())
}

#[tokio::test]
async fn pending_handler_is_dropped_before_resource_cleanup() -> TestResult {
    forced(App::new().route("/", get(blocked)), false).await
}

#[tokio::test]
async fn streaming_body_is_dropped_before_resource_cleanup() -> TestResult {
    forced(App::new().route("/", get(streaming)), false).await
}

#[tokio::test]
async fn websocket_is_dropped_before_resource_cleanup() -> TestResult {
    forced(App::new().route("/", get(upgraded)), true).await
}

#[tokio::test]
async fn cancelled_server_waits_for_abort_before_cleanup() -> TestResult {
    let probe = Probe::default();
    let run = start(App::new().route("/", get(blocked)), &probe).await?;
    let _socket = request(run.addr, false).await?;
    reached(&probe.started).await?;
    run.task.abort();
    assert!(run.task.await.is_err());
    reached(&probe.cleaned).await?;
    assert!(probe.clean_order.load(Ordering::SeqCst));
    Ok(())
}

#[tokio::test]
async fn websocket_capacity_rejects_another_upgrade() -> TestResult {
    let probe = Probe::default();
    let limits = ServerLimits {
        max_websockets: 1,
        ..ServerLimits::default()
    };
    let app = App::new().server_limits(limits).route("/", get(upgraded));
    let run = start(app, &probe).await?;
    let _first = request(run.addr, true).await?;
    reached(&probe.started).await?;
    let mut second = request(run.addr, true).await?;
    let mut buffer = [0; 512];
    let reading = second.read(&mut buffer);
    let read = tokio::time::timeout(Duration::from_secs(2), reading).await??;
    assert!(std::str::from_utf8(&buffer[..read])?.starts_with("HTTP/1.1 503"));
    let _ = run.stop.send(());
    assert!(matches!(run.task.await?, Err(ServerError::ShutdownTimeout)));
    Ok(())
}

#[tokio::test]
async fn normal_request_drains_and_closes_keep_alive() -> TestResult {
    let probe = Probe::default();
    let run = start(App::new().route("/", get(complete)), &probe).await?;
    let mut socket = request(run.addr, false).await?;
    reached(&probe.started).await?;
    let _ = run.stop.send(());
    tokio::time::timeout(Duration::from_secs(2), run.task).await???;
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await?;
    assert!(std::str::from_utf8(&response)?.contains("complete"));
    assert!(probe.clean_order.load(Ordering::SeqCst));
    Ok(())
}

struct AbortClient(tokio::task::JoinHandle<()>);

impl Drop for AbortClient {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn http2_stream_is_owned_through_forced_shutdown() -> TestResult {
    let probe = Probe::default();
    let run = start(App::new().route("/", get(blocked)), &probe).await?;
    let socket = TcpStream::connect(run.addr).await?;
    let io = TokioIo::new(socket);
    let executor = hyper_util::rt::TokioExecutor::new();
    let (mut sender, connection) = h2_handshake(executor, io).await?;
    let _client = AbortClient(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let request = http::Request::builder()
        .uri("http://localhost/")
        .body(http_body_util::Empty::<Bytes>::new())?;
    let response = async move { sender.send_request(request).await };
    let pending = tokio::spawn(response);
    reached(&probe.started).await?;
    let _ = run.stop.send(());
    assert!(matches!(run.task.await?, Err(ServerError::ShutdownTimeout)));
    assert!(probe.clean_order.load(Ordering::SeqCst));
    assert!(
        tokio::time::timeout(Duration::from_secs(2), pending)
            .await??
            .is_err()
    );
    Ok(())
}

async fn reached(notify: &Notify) -> TestResult {
    tokio::time::timeout(Duration::from_secs(2), notify.notified()).await?;
    Ok(())
}

#[tokio::test]
async fn socket_capacity_recovers_after_idle_peer_disconnects() -> TestResult {
    let probe = Probe::default();
    let limits = ServerLimits {
        max_connections: 1,
        ..ServerLimits::default()
    };
    let app = App::new().server_limits(limits).route("/", get(complete));
    let run = start(app, &probe).await?;
    let mut first = request(run.addr, false).await?;
    reached(&probe.started).await?;
    let mut buffer = [0; 512];
    assert!(first.read(&mut buffer).await? > 0);
    let mut second = request(run.addr, false).await?;
    let pending = second.read(&mut buffer);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), pending)
            .await
            .is_err()
    );
    drop(first);
    reached(&probe.started).await?;
    let reading = second.read(&mut buffer);
    assert!(tokio::time::timeout(Duration::from_secs(2), reading).await?? > 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 2);
    let _ = run.stop.send(());
    run.task.await??;
    Ok(())
}

#[test]
fn invalid_transport_limits_fail_without_panicking() {
    let invalid = [
        ServerLimits {
            max_connections: 0,
            ..ServerLimits::default()
        },
        ServerLimits {
            max_http2_streams: 0,
            ..ServerLimits::default()
        },
        ServerLimits {
            max_connections: usize::MAX,
            ..ServerLimits::default()
        },
        ServerLimits {
            max_websockets: usize::MAX,
            ..ServerLimits::default()
        },
    ];
    for limits in invalid {
        assert!(matches!(
            App::new().server_limits(limits).build(),
            Err(ServerError::Configuration(_))
        ));
    }
}

async fn reject_startup() -> Result<(), crate::ApiError> {
    Err(crate::ApiError::bad_request("startup failure"))
}

#[tokio::test]
async fn liveness_answers_while_listening_and_not_after() -> TestResult {
    let probe = Probe::default();
    let app = App::new().routes([crate::health::liveness("/livez")]);
    let run = start(app, &probe).await?;
    assert!(run.readiness.is_ready());
    let mut socket = TcpStream::connect(run.addr).await?;
    let ask = "GET /livez HTTP/1.1\r\nHost: localhost\r\n\
               Connection: close\r\n\r\n";
    socket.write_all(ask.as_bytes()).await?;
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await?;
    let text = std::str::from_utf8(&response)?;
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.contains("cache-control: no-store"), "{text}");
    assert!(text.ends_with(r#"{"status":"alive"}"#), "{text}");
    let _ = run.stop.send(());
    tokio::time::timeout(Duration::from_secs(2), run.task).await???;
    assert_eq!(run.readiness.phase(), ServerPhase::Stopped);
    let after = TcpStream::connect(run.addr).await;
    assert!(after.is_err(), "nothing listens after shutdown");
    Ok(())
}
