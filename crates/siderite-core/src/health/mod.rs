//! Opt-in health routes: process liveness
//! (`App::routes([liveness("/livez")])`) and readiness with bounded
//! application checks ([`readiness`]).
//!
//! The route answers `200` with `{"status":"alive"}` and
//! `Cache-Control: no-store` whenever the server can answer at all, in
//! every lifecycle phase, and reads nothing else: no database, no
//! readiness, no version or host details. It is registered only where the
//! application asks, so it never collides with an application route (a
//! path used twice is rejected when the app is built, like any duplicate).
//!
//! Liveness says the process still serves requests; it is not readiness.
//! The server binds only after startup completes and stops accepting new
//! connections when shutdown begins, so a probe gets no connection before
//! startup and after shutdown starts (requests already on a connection
//! are still answered while it drains). Size the probe's failure
//! threshold to cover the shutdown timeout so a draining process is not
//! restarted.

use crate::responses::WithHeaders;
use crate::{Json, MethodRouter, Route, get};
use serde::Serialize;
use serde_json::json;
use siderite_validation::{Dump, Schema, SchemaObject, SchemaRegistry};

/// The liveness answer: always `{"status":"alive"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Alive {
    /// Always `alive`.
    pub status: &'static str,
}

impl Alive {
    /// The one value a liveness route answers with.
    pub const ALIVE: Self = Self { status: "alive" };
}

impl Dump for Alive {}

impl Schema for Alive {
    fn schema_name() -> Option<&'static str> {
        Some("Alive")
    }

    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        let properties = json!({
            "status": { "type": "string", "const": "alive" },
        });
        SchemaObject::of_type("object")
            .with("properties", properties)
            .with("required", json!(["status"]))
    }
}

async fn alive() -> WithHeaders<Json<Alive>> {
    WithHeaders::new(Json(Alive::ALIVE)).header("cache-control", "no-store")
}

/// The liveness handler, documented under the `health` tag with the
/// operation ID `liveness`; mount it with [`liveness`] or
/// `App::route(path, liveness_router())`.
pub fn liveness_router() -> MethodRouter {
    get(alive)
        .operation_id("liveness")
        .tag("health")
        .summary("Whether the process is serving requests.")
        .response_model::<Alive>()
}

/// A `GET path` liveness route for `App::routes`.
///
/// Args:
///   path: Where to answer, for example `/livez`.
///
/// Returns:
///   The route; nothing is registered until the app adds it.
pub fn liveness(path: &'static str) -> Route {
    Route::new(path, liveness_router())
}

mod ready;

pub use ready::{CheckError, CheckFuture, CheckStatus, ReadyReport};
pub use ready::{ReadinessChecks, readiness, readiness_router, sql_check};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_ready;
