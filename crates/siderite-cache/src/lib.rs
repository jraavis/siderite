//! Cache backends and HTTP route caching for siderite applications.
//!
//! [`MemoryCache`] is a process-local LRU with per-entry TTL.
//! [`RedisCache`] (feature `redis`) wraps `siderite_backends::redis::RedisStore`.
//! [`RouteCache`] is a `tower` layer for `App::layer` that caches GET/HEAD 200
//! responses.
//!
//! ```
//! use siderite_cache::{Cache, CacheExt, MemoryCache};
//! use std::time::Duration;
//!
//! # #[tokio::main]
//! # async fn main() {
//! let cache = MemoryCache::new(128);
//! cache.set("greeting", b"hello".to_vec(), None).await.unwrap();
//! assert_eq!(
//!     cache.get("greeting").await.unwrap().as_deref(),
//!     Some(&b"hello"[..])
//! );
//! cache
//!     .set_json("n", &7u32, Some(Duration::from_secs(60)))
//!     .await
//!     .unwrap();
//! assert_eq!(cache.get_json::<u32>("n").await.unwrap(), Some(7));
//! # }
//! ```
#![forbid(unsafe_code)]

mod cache;
mod error;
mod memory;
mod route;

#[cfg(feature = "redis")]
mod redis;

pub use cache::{Cache, CacheExt};
pub use error::CacheError;
pub use memory::{MemoryCache, MemoryCacheLimits};
pub use route::{DEFAULT_GENERATION_TTL, DEFAULT_MAX_BODY_BYTES, RouteCache};

#[cfg(feature = "redis")]
pub use redis::RedisCache;
