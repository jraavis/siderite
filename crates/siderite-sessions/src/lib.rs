//! Cookie sessions and CSRF tokens for siderite applications.
//!
//! This crate is an adapter: it owns cookie mechanics, id generation,
//! rotation, expiry and CSRF tokens, and nothing about identity. Users,
//! login flows and password storage stay in the application, which calls
//! [`Session::login`] once it has authenticated someone. Nothing in
//! `siderite-core` depends on it.
//!
//! - [`SessionLayer`] loads the session named by the cookie, exposes it as
//!   the [`Session`] extractor and writes changes back.
//! - [`SessionConfig`] sets cookie flags (`__Host-` name, `HttpOnly`,
//!   `Secure`, `Path=/`, `SameSite=Lax`), lifetimes and the CSRF key.
//! - [`SessionStore`] is the storage trait. Stores only ever see the
//!   SHA-256 hex of a session id ([`id_hash`]). [`MemoryStore`] is
//!   included; [`CacheStore`] (feature `cache`) uses any
//!   `siderite_cache::Cache`, Redis included.
//!
//! The requirements this crate implements are in
//! `docs/plans/SESSIONS_CSRF.md`. For cross-site request forgery pair it
//! with [`siderite_core::middleware::CsrfOrigin`]; add a CSRF token for
//! forms and for `SameSite=None`.
//!
//! ```no_run
//! use siderite_core::{App, get, post};
//! use siderite_sessions::{
//!     MemoryStore, Session, SessionConfig, SessionLayer,
//! };
//!
//! async fn login(session: Session) -> &'static str {
//!     // ...authenticate the user first...
//!     session.login("ada");
//!     "welcome"
//! }
//!
//! async fn me(session: Session) -> String {
//!     session.user().unwrap_or_else(|| "anonymous".into())
//! }
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let store = MemoryStore::default();
//! let layer = SessionLayer::new(store, SessionConfig::new())?;
//! App::new()
//!     .route("/login", post(login))
//!     .route("/me", get(me))
//!     .layer(layer)
//!     .run("0.0.0.0:8000")
//!     .await?;
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]

#[cfg(feature = "cache")]
mod cache_store;
mod config;
mod id;
mod layer;
mod memory;
mod session;
mod store;
mod token;

#[cfg(feature = "cache")]
pub use cache_store::CacheStore;
pub use config::{SessionConfig, SessionConfigError};
pub use id::id_hash;
pub use layer::SessionLayer;
pub use memory::MemoryStore;
pub use session::Session;
pub use siderite_core::SameSite;
pub use store::{SessionRecord, SessionStore, StoreError};
