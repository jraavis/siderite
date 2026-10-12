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
pub(crate) fn standard(app: Option<&tools::App>) -> Server {
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

const USAGE: &str = "usage: mcp [--app [--live --database ALIAS...]]";

/// Parsed `mcp` arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Options {
    /// `--app`: run the selected application for project tools.
    pub app: bool,
    /// `--live` with its `--database` aliases.
    pub live: Vec<String>,
}

/// Parse the arguments after `mcp`.
///
/// Errors:
///   A usage error for an unknown argument, `--live` without `--app` or
///   without an alias, or an alias without `--live`.
pub(crate) fn parse(raw: &[String]) -> Result<Options, CliError> {
    let mut options = Options::default();
    let (mut live, mut aliases) = (false, Vec::new());
    let mut args = raw.iter().skip_while(|a| *a != "mcp").skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--app" => options.app = true,
            "--live" => live = true,
            "--database" => {
                let usage = || CliError::usage(USAGE);
                aliases.push(args.next().ok_or_else(usage)?.clone());
            }
            other => {
                let msg = format!("unexpected argument `{other}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
        }
    }
    if live != !aliases.is_empty() || (live && !options.app) {
        let msg = format!(
            "live tools need --app, --live and at least one --database \
             ALIAS together; {USAGE}"
        );
        return Err(CliError::usage(msg));
    }
    options.live = aliases;
    Ok(options)
}

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
    let options = parse(raw)?;
    let app = if options.app {
        let found = crate::target::locate("cargo", cwd, project, true)?;
        let missing = || CliError::usage("--app needs an application package");
        let found = found.ok_or_else(missing)?;
        let bin = found.bin.or(found.default_run);
        let bridge = Arc::new(Bridge::new(found.dir, bin));
        Some(tools::App {
            bridge,
            live: options.live,
        })
    } else {
        None
    };
    let (input, output) = (tokio::io::stdin(), tokio::io::stdout());
    let server = standard(app.as_ref());
    serve(server, input, output, Limits::default()).await?;
    Ok(0)
}
