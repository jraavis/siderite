//! Layered configuration for siderite applications.
//!
//! Precedence, lowest to highest: compiled defaults, an optional TOML file,
//! environment variables, programmatic [`ConfigBuilder::set`] overrides.
//!
//! ```
//! use siderite_config::ConfigBuilder;
//!
//! let settings = ConfigBuilder::new()
//!     .set("app.name", "demo")
//!     .build()
//!     .unwrap();
//! assert_eq!(settings.app.name, "demo");
//! assert_eq!(settings.server.addr, "127.0.0.1:8000");
//! ```
#![forbid(unsafe_code)]

mod builder;
mod error;
mod secret;
mod settings;
mod tracing_init;

pub use builder::{ConfigBuilder, DEFAULT_CONFIG_FILE, DEFAULT_ENV_PREFIX, load};
pub use error::{ConfigError, is_secret_key};
pub use secret::Secret;
pub use settings::{
    AppSettings, CacheSettings, DEFAULT_APP_NAME, DEFAULT_CACHE_MAX_ENTRIES, DEFAULT_LOG_LEVEL,
    DEFAULT_SERVER_ADDR, DatabaseSettings, LogSettings, ServerSettings, Settings,
};
pub use tracing_init::init_tracing;

#[cfg(test)]
mod tests;
