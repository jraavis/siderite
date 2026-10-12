//! `siderite mcp`: a Model Context Protocol server on stdin and stdout,
//! built only with the `mcp` feature.
//!
//! The transport and protocol negotiation come from the official Rust SDK
//! (`rmcp`); every protocol revision it knows is offered, and it answers
//! `initialize` with the newest revision both sides share. Stdout carries
//! protocol messages only: the server writes nothing else there, and its
//! own messages go to stderr, redacted. There is no network listener and
//! no tool that runs commands or touches files. Each message is at most
//! [`Limits::max_message`] bytes (a larger one ends the session), each
//! tool call is bounded in time and result size, and a cancelled call is
//! dropped. The session ends when the client closes stdin.

use crate::error::CliError;

pub(crate) mod bridge;
pub(crate) mod limits;
pub(crate) mod server;
#[cfg(test)]
mod tests;
pub(crate) mod tools;

use bridge::Bridge;
use limits::{Limits, LineLimit};
use rmcp::ServiceExt;
use server::Server;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The server with the standard tools and limits; the project tools
/// only for an `app`.
pub(crate) fn standard(app: Option<Arc<Bridge>>) -> Server {
    let limits = Limits::default();
    Server::new(tools::all(app, limits), limits)
}

/// Serve `server` over `input` and `output` until the client closes the
/// input or a message breaks the limits.
///
/// Errors:
///   An IO error when the session cannot start or ends abnormally.
pub(crate) async fn serve<R, W>(
    server: Server,
    input: R,
    output: W,
    limits: Limits,
) -> Result<(), CliError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let input = LineLimit::new(input, limits.max_message);
    let tripped = input.tripped();
    let running = server.serve((input, output)).await;
    let running = running.map_err(|e| failed(&e.to_string()))?;
    let ended = running.waiting().await;
    ended.map_err(|e| failed(&e.to_string()))?;
    if tripped.load(Ordering::Relaxed) {
        let max = limits.max_message;
        return Err(failed(&format!("a message exceeds {max} bytes")));
    }
    Ok(())
}

fn failed(why: &str) -> CliError {
    let why = crate::report::redact_text(why);
    CliError::Io(format!("mcp session ended: {why}"))
}

const USAGE: &str = "usage: mcp [--app]";

/// `siderite mcp [--app]` from `cwd`: with `--app`, the selected
/// application package (project flags apply) answers the project tools.
///
/// Errors:
///   A usage error for another argument or no application to select,
///   or a session that fails.
pub(crate) async fn run(
    raw: &[String],
    cwd: &std::path::Path,
    project: &crate::target::ProjectArgs,
) -> Result<u8, CliError> {
    let mut app = false;
    for arg in raw.iter().skip_while(|a| *a != "mcp").skip(1) {
        if arg != "--app" {
            let msg = format!("unexpected argument `{arg}`; {USAGE}");
            return Err(CliError::usage(msg));
        }
        app = true;
    }
    let bridge = if app {
        let found = crate::target::locate("cargo", cwd, project, true)?;
        let missing = || CliError::usage("--app needs an application package");
        let found = found.ok_or_else(missing)?;
        let bin = found.bin.or(found.default_run);
        Some(Arc::new(Bridge::new(found.dir, bin)))
    } else {
        None
    };
    let (input, output) = (tokio::io::stdin(), tokio::io::stdout());
    serve(standard(bridge), input, output, Limits::default()).await?;
    Ok(0)
}
