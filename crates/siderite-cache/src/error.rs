//! [`CacheError`] and conversion helpers.

use thiserror::Error;

/// Failure from a [`Cache`](crate::Cache) operation.
///
/// Messages never include cached values, passwords, tokens, API keys or other
/// secrets. Keys may appear because they identify the entry; do not put
/// secrets in cache keys.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CacheError {
    /// Configured key, value or total byte admission was exceeded.
    #[error("cache byte admission limit exceeded")]
    SizeLimit,
    /// Cache limits must be positive and fit within the total byte budget.
    #[error("invalid cache byte limits")]
    InvalidLimits,
    /// A TTL is zero where a positive lifetime is required, or cannot be
    /// represented by the backend's monotonic deadline.
    #[error("cache TTL is not representable")]
    InvalidTtl,
    /// The backend does not implement an optional [`Cache`](crate::Cache)
    /// operation. Callers must fail open or bypass; they must not emulate
    /// the operation with non-atomic calls.
    #[error("cache operation `{operation}` is unsupported by this backend")]
    Unsupported {
        /// Name of the unsupported trait method.
        operation: &'static str,
    },
    /// The backend could not complete the operation.
    #[error("cache backend error: {0}")]
    Backend(String),
    /// [`Cache::increment`](crate::Cache::increment) found a non-integer value,
    /// or the result would overflow a signed 64-bit integer.
    #[error("cache key `{key}` is not an integer or the increment overflowed")]
    NotInteger {
        /// The cache key (not the stored value).
        key: String,
    },
    /// JSON serialization failed before anything was stored.
    #[error("cannot serialize cache value for `{key}`: {reason}")]
    Serialize {
        /// The cache key.
        key: String,
        /// Why serialization failed.
        reason: String,
    },
    /// A stored value could not be decoded as JSON of the requested type.
    #[error("cannot deserialize cache value for `{key}`: {reason}")]
    Deserialize {
        /// The cache key.
        key: String,
        /// Why deserialization failed.
        reason: String,
    },
}
