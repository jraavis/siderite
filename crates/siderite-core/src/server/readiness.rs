//! Application readiness for probes during startup and resource teardown.

use crate::{ApiError, FromRequestParts};
use http::request::Parts;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Observable phase of the root application lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerPhase {
    /// Initialization has not completed.
    Starting,
    /// Startup has completed and admission is available.
    Ready,
    /// Transport/task admission is closing and resources are draining.
    Draining,
    /// Cleanup attempts have completed, successfully or with an error.
    Stopped,
}

/// Extractor and shareable handle for application readiness probes.
///
/// In-process services built without running lifespan remain Starting.
/// Readiness does not assess database health or prove a load balancer has
/// removed this server; deployments must configure their probes and grace.
#[derive(Debug, Clone)]
pub struct Readiness(Arc<AtomicU8>);

impl Readiness {
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicU8::new(0)))
    }

    /// Read the current root application lifecycle phase.
    ///
    /// Returns:
    ///     A snapshot which can change immediately after this call.
    pub fn phase(&self) -> ServerPhase {
        match self.0.load(Ordering::Acquire) {
            0 => ServerPhase::Starting,
            1 => ServerPhase::Ready,
            2 => ServerPhase::Draining,
            _ => ServerPhase::Stopped,
        }
    }

    /// Return whether startup completed and shutdown has not begun.
    ///
    /// Returns:
    ///     True only during the Ready phase.
    pub fn is_ready(&self) -> bool {
        self.phase() == ServerPhase::Ready
    }

    pub(crate) fn ready(&self) {
        self.0.store(1, Ordering::Release);
    }

    pub(crate) fn draining(&self) {
        // Repeated cleanup must not move Stopped back into Draining.
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |phase| {
                (phase < 2).then_some(2)
            });
    }

    pub(crate) fn stopped(&self) {
        self.0.store(3, Ordering::Release);
    }
}

impl FromRequestParts for Readiness {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or_else(|| ApiError::internal("readiness is not installed"))
    }
}
