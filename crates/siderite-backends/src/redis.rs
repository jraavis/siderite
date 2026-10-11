//! Redis key, hash and set store.
//!
//! [`RedisStore`] wraps a cloneable [`redis::aio::ConnectionManager`]. It is a
//! specialised data API: relational plans are executed by the SQLite and
//! PostgreSQL adapters. [`RedisStore::capabilities`] rejects every relational
//! [`Feature`] before any I/O.
//!
//! Keys are strings. A prefix (for example `"cache:"`) is prepended as-is, so
//! include the separator in the prefix when one is wanted. Values are UTF-8.
//! Time-to-live is expressed in milliseconds (`PX` / `PEXPIRE` / `PTTL`); a
//! duration below one millisecond is rejected before a command is sent.
//!
//! ```
//! use siderite_backends::redis::RedisStore;
//! use siderite_orm::{BackendKind, Feature, OrmError};
//!
//! let err = RedisStore::require(Feature::Joins).unwrap_err();
//! assert!(matches!(
//!     OrmError::from(err),
//!     OrmError::Capability(siderite_orm::BackendCapabilityError::Unsupported {
//!         backend: BackendKind::Redis,
//!         feature: Feature::Joins,
//!     })
//! ));
//! ```

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use siderite_orm::{
    BackendCapabilities, BackendCapabilityError, BackendError, Feature, OrmError, QueryError,
};
use thiserror::Error;

/// Largest TTL accepted, in milliseconds (`i64::MAX / 2`).
///
/// Redis rejects a relative TTL whose absolute deadline (now plus the TTL)
/// overflows a signed 64-bit millisecond count. Half of `i64::MAX` leaves
/// room for any realistic clock value, so it is validated up front.
const MAX_TTL_MS: u64 = (i64::MAX / 2) as u64;

/// Failure from [`RedisStore`].
///
/// Converts into [`OrmError`]: connection and command failures become
/// [`BackendError`], JSON and UTF-8 problems become [`QueryError::Decode`],
/// rejected arguments become [`QueryError::InvalidPlan`], and capability
/// mismatches stay [`BackendCapabilityError`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RedisError {
    /// The URL was rejected or the connection could not be established.
    #[error("redis connection error: {0}")]
    Connection(String),
    /// Redis rejected a command, or the reply was not the one this client expects.
    #[error("redis error: {0}")]
    Command(String),
    /// A stored value was not valid UTF-8 or JSON.
    #[error("cannot decode redis value `{key}`: {reason}")]
    Decode {
        /// Logical key (without applying a second prefix).
        key: String,
        /// What was wrong with the bytes.
        reason: String,
    },
    /// The call was refused before any command was sent.
    #[error("{0}")]
    Invalid(String),
    /// A relational feature Redis does not provide.
    #[error(transparent)]
    Capability(#[from] BackendCapabilityError),
}

impl From<redis::RedisError> for RedisError {
    fn from(err: redis::RedisError) -> Self {
        match err.kind() {
            redis::ErrorKind::Io
            | redis::ErrorKind::Client
            | redis::ErrorKind::AuthenticationFailed
            | redis::ErrorKind::InvalidClientConfig
            | redis::ErrorKind::ClusterConnectionNotFound
            | redis::ErrorKind::MasterNameNotFoundBySentinel
            | redis::ErrorKind::NoValidReplicasFoundBySentinel
            | redis::ErrorKind::EmptySentinelList => Self::Connection(err.to_string()),
            _ => Self::Command(err.to_string()),
        }
    }
}

impl From<RedisError> for OrmError {
    fn from(err: RedisError) -> Self {
        match err {
            RedisError::Connection(message) => Self::Backend(BackendError::Connection(message)),
            RedisError::Command(message) => Self::Backend(BackendError::Database(message)),
            RedisError::Decode { key, reason } => Self::Query(QueryError::Decode {
                column: key,
                reason,
            }),
            RedisError::Invalid(message) => Self::Query(QueryError::InvalidPlan(message)),
            RedisError::Capability(err) => Self::Capability(err),
        }
    }
}

/// Remaining lifetime of a key, from `PTTL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ttl {
    /// The key does not exist (`PTTL` returned -2).
    Missing,
    /// The key exists and has no expiry (`PTTL` returned -1).
    Persistent,
    /// Milliseconds Redis still has left on the key.
    ExpiresIn(Duration),
}

/// Cloneable Redis client.
///
/// Cloning copies the prefix and an `Arc` inside [`redis::aio::ConnectionManager`];
/// it does not open another TCP connection. Commands issued on any clone share
/// that multiplexed connection and the database selected by the URL.
#[derive(Clone)]
pub struct RedisStore {
    manager: redis::aio::ConnectionManager,
    prefix: String,
}

impl std::fmt::Debug for RedisStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisStore")
            .field("prefix", &self.prefix)
            .field("database", &self.database())
            .finish()
    }
}

impl RedisStore {
    /// Capabilities of this adapter.
    ///
    /// The value does not depend on a connection. Checking it never performs I/O.
    pub fn capabilities() -> BackendCapabilities {
        BackendCapabilities::redis()
    }

    /// Reject `feature` when Redis cannot honour it.
    ///
    /// No connection is used. Isolation levels, savepoints, joins, row locking
    /// and the other relational [`Feature`]s all fail here.
    ///
    /// # Errors
    /// [`RedisError::Capability`] when `feature` is not supported.
    pub fn require(feature: Feature) -> Result<(), RedisError> {
        Self::capabilities().require(feature).map_err(Into::into)
    }

    /// Open `url` (for example `redis://127.0.0.1:6379/15`) with an empty prefix.
    ///
    /// The path selects the database. Database `15` is the one the live tests use.
    ///
    /// # Errors
    /// [`RedisError::Connection`] when the URL is invalid or the server cannot be reached.
    pub async fn connect(url: &str) -> Result<Self, RedisError> {
        let client = redis::Client::open(url)?;
        Self::connect_with(client, redis::aio::ConnectionManagerConfig::new()).await
    }

    /// Connect a configured client, including custom TLS trust roots.
    ///
    /// Args:
    ///     client: Driver client with transport, certificate and DB policy.
    ///     config: Retry, timeout and multiplexed admission limits.
    ///
    /// Returns:
    ///     Connected store with an empty key prefix.
    ///
    /// # Errors
    /// Connection, authentication or certificate verification failure.
    pub async fn connect_with(
        client: redis::Client,
        config: redis::aio::ConnectionManagerConfig,
    ) -> Result<Self, RedisError> {
        let manager = redis::aio::ConnectionManager::new_with_config(client, config).await?;
        Ok(Self {
            manager,
            prefix: String::new(),
        })
    }

    /// Clone this client with `prefix` replacing the current namespace.
    ///
    /// The prefix is concatenated in front of every logical key. It is not
    /// appended to the previous prefix. `"app:"` turns `"user"` into `"app:user"`.
    /// An empty prefix stores logical keys unchanged.
    pub fn with_prefix(&self, prefix: impl Into<String>) -> Self {
        Self {
            manager: self.manager.clone(),
            prefix: prefix.into(),
        }
    }

    /// Namespace prepended to logical keys.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Physical key stored in Redis for the logical `key`.
    pub fn key(&self, key: &str) -> String {
        prefixed_key(&self.prefix, key)
    }

    /// Database index selected by the connection URL.
    ///
    /// This is the index the client was configured with. It is not a live `SELECT`.
    pub fn database(&self) -> i64 {
        redis::aio::ConnectionLike::get_db(&self.manager)
    }

    /// `GET`. `Ok(None)` when the key is absent.
    ///
    /// # Errors
    /// Connection, command, or UTF-8 decode errors. A wrong-type key is a command error.
    pub async fn get(&self, key: &str) -> Result<Option<String>, RedisError> {
        let raw: Option<Vec<u8>> = self.query(redis::cmd("GET").arg(self.key(key))).await?;
        optional_utf8(key, raw)
    }

    /// `SET`. `ttl` of `None` stores the key with no expiry and clears any previous one.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `ttl` is below 1ms or above `i64::MAX / 2` milliseconds,
    /// before any command is sent. Otherwise connection or command errors.
    pub async fn set(
        &self,
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<(), RedisError> {
        let px = optional_ttl(ttl)?;
        let wrote = self.set_raw(key, value, px, false).await?;
        if wrote {
            Ok(())
        } else {
            Err(RedisError::Command(format!(
                "SET `{key}` returned a nil reply"
            )))
        }
    }

    /// `SET key value NX`. `Ok(true)` when this call created the key.
    ///
    /// An existing key is left unchanged, including its expiry. This form does
    /// not set a TTL; use [`set`](Self::set) or [`expire`](Self::expire), or a
    /// [`pipeline`](Self::pipeline) `SET` with `NX` and `PX`, when the new key
    /// must expire.
    ///
    /// # Errors
    /// Connection or command errors.
    pub async fn set_nx(&self, key: &str, value: &str) -> Result<bool, RedisError> {
        self.set_raw(key, value, None, true).await
    }

    /// `SET key value NX PX ms` as one command. `Ok(true)` when this call
    /// created the key; an existing key keeps its value and expiry.
    ///
    /// Creation and expiry are one atomic server step: no observer sees the
    /// key without its TTL, and dropping the returned future either sends the
    /// whole command or none of it. `ttl` is truncated to whole milliseconds.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `ttl` is below 1ms or above
    /// `i64::MAX / 2` milliseconds, before any command is sent. Otherwise connection or
    /// command errors.
    pub async fn set_nx_px(
        &self,
        key: &str,
        value: &str,
        ttl: Duration,
    ) -> Result<bool, RedisError> {
        let px = required_ttl(ttl)?;
        self.set_raw(key, value, Some(px), true).await
    }

    /// `DEL` for one logical key. The number is `1` when the key was removed and `0` otherwise.
    ///
    /// # Errors
    /// Connection or command errors.
    pub async fn del(&self, key: &str) -> Result<u64, RedisError> {
        let removed: i64 = self.query(redis::cmd("DEL").arg(self.key(key))).await?;
        as_count(removed)
    }

    /// `EXISTS` for one logical key.
    ///
    /// # Errors
    /// Connection or command errors.
    pub async fn exists(&self, key: &str) -> Result<bool, RedisError> {
        let n: i64 = self.query(redis::cmd("EXISTS").arg(self.key(key))).await?;
        Ok(n != 0)
    }

    /// `PEXPIRE`. `Ok(false)` when the key does not exist.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `ttl` is below 1ms or too large, before any
    /// command is sent. Otherwise connection or command errors.
    pub async fn expire(&self, key: &str, ttl: Duration) -> Result<bool, RedisError> {
        let ms = required_ttl(ttl)?;
        let n: i64 = self
            .query(redis::cmd("PEXPIRE").arg(self.key(key)).arg(ms))
            .await?;
        Ok(n != 0)
    }

    /// `PTTL`.
    ///
    /// # Errors
    /// Connection or command errors, including an unexpected numeric reply.
    pub async fn ttl(&self, key: &str) -> Result<Ttl, RedisError> {
        let ms: i64 = self.query(redis::cmd("PTTL").arg(self.key(key))).await?;
        match ms {
            -2 => Ok(Ttl::Missing),
            -1 => Ok(Ttl::Persistent),
            0.. => Ok(Ttl::ExpiresIn(Duration::from_millis(
                u64::try_from(ms)
                    .map_err(|_| RedisError::Command(format!("PTTL `{key}` overflowed: {ms}")))?,
            ))),
            other => Err(RedisError::Command(format!(
                "unexpected PTTL reply for `{key}`: {other}"
            ))),
        }
    }

    /// `INCRBY`. Creates the key at zero first when it is absent. `by` may be negative.
    ///
    /// # Errors
    /// Connection or command errors (the value is not an integer, or it would overflow).
    pub async fn incr_by(&self, key: &str, by: i64) -> Result<i64, RedisError> {
        self.query(redis::cmd("INCRBY").arg(self.key(key)).arg(by))
            .await
    }

    /// `HGET`. `Ok(None)` when the key or field is absent.
    ///
    /// # Errors
    /// Connection, command, or UTF-8 decode errors.
    pub async fn hget(&self, key: &str, field: &str) -> Result<Option<String>, RedisError> {
        let raw: Option<Vec<u8>> = self
            .query(redis::cmd("HGET").arg(self.key(key)).arg(field))
            .await?;
        optional_utf8(key, raw)
    }

    /// `HSET` of one field. Returns how many fields were newly created (`0` when updated).
    ///
    /// # Errors
    /// Connection or command errors.
    pub async fn hset(&self, key: &str, field: &str, value: &str) -> Result<u64, RedisError> {
        let n: i64 = self
            .query(redis::cmd("HSET").arg(self.key(key)).arg(field).arg(value))
            .await?;
        as_count(n)
    }

    /// `HSET` of many fields in one command. Returns how many fields were newly created.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `fields` is empty, before any command is sent.
    /// Otherwise connection or command errors.
    pub async fn hset_many<I, F, V>(&self, key: &str, fields: I) -> Result<u64, RedisError>
    where
        I: IntoIterator<Item = (F, V)>,
        F: AsRef<str>,
        V: AsRef<str>,
    {
        let fields = collect_pairs(fields, "hset_many requires at least one field")?;
        let mut cmd = redis::cmd("HSET");
        cmd.arg(self.key(key));
        for (field, value) in &fields {
            cmd.arg(field).arg(value);
        }
        as_count(self.query(&cmd).await?)
    }

    /// `HGETALL`. An absent key is an empty map, which is how Redis replies.
    ///
    /// Field order is not significant.
    ///
    /// # Errors
    /// Connection, command, or UTF-8 decode errors.
    pub async fn hgetall(&self, key: &str) -> Result<HashMap<String, String>, RedisError> {
        let raw: Vec<(Vec<u8>, Vec<u8>)> =
            self.query(redis::cmd("HGETALL").arg(self.key(key))).await?;
        let mut out = HashMap::with_capacity(raw.len());
        for (field, value) in raw {
            out.insert(decode_utf8(key, field)?, decode_utf8(key, value)?);
        }
        Ok(out)
    }

    /// `HDEL`. Returns how many of `fields` were removed.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `fields` is empty, before any command is sent.
    /// Otherwise connection or command errors.
    pub async fn hdel<I, F>(&self, key: &str, fields: I) -> Result<u64, RedisError>
    where
        I: IntoIterator<Item = F>,
        F: AsRef<str>,
    {
        let fields = collect_strings(fields, "hdel requires at least one field")?;
        let mut cmd = redis::cmd("HDEL");
        cmd.arg(self.key(key));
        for field in &fields {
            cmd.arg(field);
        }
        as_count(self.query(&cmd).await?)
    }

    /// `SADD`. Returns how many of `members` were newly inserted.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `members` is empty, before any command is sent.
    /// Otherwise connection or command errors.
    pub async fn sadd<I, M>(&self, key: &str, members: I) -> Result<u64, RedisError>
    where
        I: IntoIterator<Item = M>,
        M: AsRef<str>,
    {
        self.set_op("SADD", "sadd requires at least one member", key, members)
            .await
    }

    /// `SREM`. Returns how many of `members` were removed.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `members` is empty, before any command is sent.
    /// Otherwise connection or command errors.
    pub async fn srem<I, M>(&self, key: &str, members: I) -> Result<u64, RedisError>
    where
        I: IntoIterator<Item = M>,
        M: AsRef<str>,
    {
        self.set_op("SREM", "srem requires at least one member", key, members)
            .await
    }

    /// `SMEMBERS`. An absent key is an empty set.
    ///
    /// # Errors
    /// Connection, command, or UTF-8 decode errors.
    pub async fn smembers(&self, key: &str) -> Result<HashSet<String>, RedisError> {
        let raw: Vec<Vec<u8>> = self
            .query(redis::cmd("SMEMBERS").arg(self.key(key)))
            .await?;
        raw.into_iter()
            .map(|bytes| decode_utf8(key, bytes))
            .collect()
    }

    /// `SISMEMBER`.
    ///
    /// # Errors
    /// Connection or command errors.
    pub async fn sismember(&self, key: &str, member: &str) -> Result<bool, RedisError> {
        let n: i64 = self
            .query(redis::cmd("SISMEMBER").arg(self.key(key)).arg(member))
            .await?;
        Ok(n != 0)
    }

    /// `GET` decoded as JSON. `Ok(None)` when the key is absent.
    ///
    /// A stored JSON `null` is `Ok(Some)` when `T` accepts null, and a decode
    /// error when it does not.
    ///
    /// # Errors
    /// [`RedisError::Decode`] when the bytes are not UTF-8 or not JSON of type `T`.
    /// Connection and command errors from [`get`](Self::get).
    pub async fn get_json<T>(&self, key: &str) -> Result<Option<T>, RedisError>
    where
        T: DeserializeOwned,
    {
        let Some(raw) = self.get(key).await? else {
            return Ok(None);
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|err| RedisError::Decode {
                key: key.to_owned(),
                reason: format!("invalid JSON: {err}"),
            })
    }

    /// `SET` of `value` encoded as compact JSON.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `value` cannot be serialized or `ttl` is not
    /// acceptable, before any command is sent. Otherwise the errors of [`set`](Self::set).
    pub async fn set_json<T>(
        &self,
        key: &str,
        value: &T,
        ttl: Option<Duration>,
    ) -> Result<(), RedisError>
    where
        T: Serialize,
    {
        let px = optional_ttl(ttl)?;
        let json = serde_json::to_string(value).map_err(|err| {
            RedisError::Invalid(format!("cannot serialize JSON for `{key}`: {err}"))
        })?;
        let wrote = self.set_raw(key, &json, px, false).await?;
        if wrote {
            Ok(())
        } else {
            Err(RedisError::Command(format!(
                "SET `{key}` returned a nil reply"
            )))
        }
    }

    /// Run `build` inside one atomic `MULTI`/`EXEC`.
    ///
    /// `build` receives a [`redis::Pipeline`] already switched to atomic mode.
    /// Keys are not prefixed for these raw commands: pass them through [`key`](Self::key).
    /// Replies decode as `T`, one element per command that was not ignored, in order.
    ///
    /// Redis runs the queued commands with no other client's commands interleaved.
    /// That is not a SQL transaction. A command that fails while `EXEC` is running
    /// does not roll back earlier commands in the same batch. A command Redis
    /// rejects while queueing (wrong arity, unknown command) aborts the whole
    /// batch, and none of it is applied. There are no savepoints and no isolation
    /// levels; ask [`require`](Self::require) before building a pipeline that
    /// would need them.
    ///
    /// # Errors
    /// Connection or command errors, including `EXECABORT` and a reply shape `T` cannot decode.
    pub async fn pipeline<T, F>(&self, build: F) -> Result<T, RedisError>
    where
        T: redis::FromRedisValue,
        F: FnOnce(&mut redis::Pipeline),
    {
        let mut pipe = redis::pipe();
        build(pipe.atomic());
        let mut conn = self.manager.clone();
        pipe.query_async(&mut conn).await.map_err(RedisError::from)
    }

    /// Delete every key in this namespace with `SCAN` and `DEL`.
    ///
    /// The match pattern is the prefix with glob metacharacters escaped, then
    /// `*`. An empty prefix is refused so this cannot wipe a database. This
    /// method never sends `FLUSHDB` or `FLUSHALL`.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when the prefix is empty, before any command is
    /// sent. Otherwise connection or command errors.
    pub async fn delete_namespace(&self) -> Result<u64, RedisError> {
        if self.prefix.is_empty() {
            return Err(RedisError::Invalid(
                "refusing to delete keys when the prefix is empty; this would match every key"
                    .into(),
            ));
        }
        let pattern = format!("{}*", glob_literal(&self.prefix));
        let mut cursor = "0".to_owned();
        let mut deleted = 0u64;
        loop {
            let (next, keys): (String, Vec<Vec<u8>>) = self
                .query(
                    redis::cmd("SCAN")
                        .arg(&cursor)
                        .arg("MATCH")
                        .arg(&pattern)
                        .arg("COUNT")
                        .arg(200u32),
                )
                .await?;
            if !keys.is_empty() {
                let mut cmd = redis::cmd("DEL");
                for key in &keys {
                    cmd.arg(key);
                }
                deleted += as_count(self.query(&cmd).await?)?;
            }
            if next == "0" {
                break;
            }
            cursor = next;
        }
        Ok(deleted)
    }

    async fn set_raw(
        &self,
        key: &str,
        value: &str,
        px: Option<u64>,
        nx: bool,
    ) -> Result<bool, RedisError> {
        let mut cmd = redis::cmd("SET");
        cmd.arg(self.key(key)).arg(value);
        if let Some(ms) = px {
            cmd.arg("PX").arg(ms);
        }
        if nx {
            cmd.arg("NX");
        }
        let reply: redis::Value = self.query(&cmd).await?;
        match reply {
            redis::Value::Nil => Ok(false),
            redis::Value::SimpleString(ref status) if status == "OK" => Ok(true),
            redis::Value::Okay => Ok(true),
            other => Err(RedisError::Command(format!(
                "unexpected SET reply for `{key}`: {other:?}"
            ))),
        }
    }

    async fn set_op<I, M>(
        &self,
        command: &str,
        empty: &str,
        key: &str,
        members: I,
    ) -> Result<u64, RedisError>
    where
        I: IntoIterator<Item = M>,
        M: AsRef<str>,
    {
        let members = collect_strings(members, empty)?;
        let mut cmd = redis::cmd(command);
        cmd.arg(self.key(key));
        for member in &members {
            cmd.arg(member);
        }
        as_count(self.query(&cmd).await?)
    }

    async fn query<T>(&self, cmd: &redis::Cmd) -> Result<T, RedisError>
    where
        T: redis::FromRedisValue,
    {
        let mut conn = self.manager.clone();
        cmd.query_async(&mut conn).await.map_err(RedisError::from)
    }
}

fn prefixed_key(prefix: &str, key: &str) -> String {
    let mut full = String::with_capacity(prefix.len() + key.len());
    full.push_str(prefix);
    full.push_str(key);
    full
}

/// Escape `*`, `?`, `[` and `\` so `SCAN MATCH` treats `prefix` as a literal.
fn glob_literal(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len());
    for ch in prefix.chars() {
        if matches!(ch, '*' | '?' | '[' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn optional_ttl(ttl: Option<Duration>) -> Result<Option<u64>, RedisError> {
    ttl.map(required_ttl).transpose()
}

fn required_ttl(ttl: Duration) -> Result<u64, RedisError> {
    let ms = ttl.as_millis();
    let Ok(ms) = u64::try_from(ms) else {
        return Err(ttl_rejected());
    };
    if ms == 0 || ms > MAX_TTL_MS {
        return Err(ttl_rejected());
    }
    Ok(ms)
}

fn ttl_rejected() -> RedisError {
    RedisError::Invalid(
        "redis TTL must be between 1 millisecond and i64::MAX / 2 milliseconds".into(),
    )
}

fn as_count(n: i64) -> Result<u64, RedisError> {
    u64::try_from(n).map_err(|_| RedisError::Command(format!("negative count from redis: {n}")))
}

fn decode_utf8(key: &str, bytes: Vec<u8>) -> Result<String, RedisError> {
    String::from_utf8(bytes).map_err(|err| RedisError::Decode {
        key: key.to_owned(),
        reason: format!("value is not UTF-8: {err}"),
    })
}

fn optional_utf8(key: &str, bytes: Option<Vec<u8>>) -> Result<Option<String>, RedisError> {
    bytes.map(|value| decode_utf8(key, value)).transpose()
}

fn collect_strings<I, S>(items: I, empty: &str) -> Result<Vec<String>, RedisError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let items: Vec<String> = items
        .into_iter()
        .map(|item| item.as_ref().to_owned())
        .collect();
    if items.is_empty() {
        Err(RedisError::Invalid(empty.to_owned()))
    } else {
        Ok(items)
    }
}

fn collect_pairs<I, F, V>(pairs: I, empty: &str) -> Result<Vec<(String, String)>, RedisError>
where
    I: IntoIterator<Item = (F, V)>,
    F: AsRef<str>,
    V: AsRef<str>,
{
    let pairs: Vec<(String, String)> = pairs
        .into_iter()
        .map(|(field, value)| (field.as_ref().to_owned(), value.as_ref().to_owned()))
        .collect();
    if pairs.is_empty() {
        Err(RedisError::Invalid(empty.to_owned()))
    } else {
        Ok(pairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siderite_orm::{BackendKind, IsolationLevel};

    #[test]
    fn key_prefix_is_prepended_and_empty_prefix_is_identity() {
        assert_eq!(prefixed_key("", "user:1"), "user:1");
        assert_eq!(prefixed_key("app:", "user:1"), "app:user:1");
        assert_eq!(prefixed_key("app:", ""), "app:");
        assert_eq!(prefixed_key("ns", "user"), "nsuser");
    }

    #[test]
    fn glob_metacharacters_in_a_prefix_are_escaped() {
        assert_eq!(glob_literal("a*b?c[d]\\e"), "a\\*b\\?c\\[d]\\\\e");
        assert_eq!(glob_literal("plain:"), "plain:");
    }

    #[test]
    fn ttl_bounds_are_rejected_before_any_connection() {
        assert!(matches!(
            required_ttl(Duration::ZERO),
            Err(RedisError::Invalid(_))
        ));
        assert!(matches!(
            optional_ttl(Some(Duration::from_nanos(1))),
            Err(RedisError::Invalid(_))
        ));
        assert_eq!(
            optional_ttl(Some(Duration::from_millis(1500))).unwrap(),
            Some(1500)
        );
        assert_eq!(optional_ttl(None).unwrap(), None);
        let too_long = Duration::from_millis(MAX_TTL_MS).saturating_add(Duration::from_millis(1));
        assert!(matches!(
            required_ttl(too_long),
            Err(RedisError::Invalid(_))
        ));
        let orm = OrmError::from(required_ttl(Duration::ZERO).unwrap_err());
        assert!(matches!(orm, OrmError::Query(QueryError::InvalidPlan(_))));
    }

    #[test]
    fn relational_features_fail_before_any_connection() {
        assert!(matches!(
            RedisStore::require(Feature::RowLocking),
            Err(RedisError::Capability(
                BackendCapabilityError::RowLockingUnsupported {
                    backend: BackendKind::Redis
                }
            ))
        ));
        let err = RedisStore::require(Feature::Joins).unwrap_err();
        assert!(matches!(
            OrmError::from(err),
            OrmError::Capability(BackendCapabilityError::Unsupported {
                backend: BackendKind::Redis,
                feature: Feature::Joins,
            })
        ));
        assert!(RedisStore::require(Feature::Savepoints).is_err());
        assert!(RedisStore::require(Feature::Isolation(IsolationLevel::Serializable)).is_err());
        assert!(!RedisStore::capabilities().supports(Feature::CaseInsensitiveLike));
    }

    #[test]
    fn driver_and_decode_errors_map_into_orm_error() {
        let connection = RedisError::Connection("refused".into());
        assert!(matches!(
            OrmError::from(connection),
            OrmError::Backend(BackendError::Connection(message)) if message == "refused"
        ));
        let command = RedisError::Command("WRONGTYPE".into());
        assert!(matches!(
            OrmError::from(command),
            OrmError::Backend(BackendError::Database(message)) if message == "WRONGTYPE"
        ));
        let decode = RedisError::Decode {
            key: "widget".into(),
            reason: "invalid JSON".into(),
        };
        assert!(matches!(
            OrmError::from(decode),
            OrmError::Query(QueryError::Decode { column, reason })
                if column == "widget" && reason == "invalid JSON"
        ));
    }

    #[test]
    fn empty_argument_lists_are_rejected_without_a_command() {
        let err = collect_strings(Vec::<&str>::new(), "sadd requires at least one member");
        assert!(matches!(err, Err(RedisError::Invalid(_))));
        let err = collect_pairs(
            Vec::<(&str, &str)>::new(),
            "hset_many requires at least one field",
        );
        assert!(matches!(err, Err(RedisError::Invalid(_))));
    }

    #[test]
    fn store_type_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RedisStore>();
    }

    #[tokio::test]
    async fn a_malformed_url_is_a_connection_error() {
        let err = RedisStore::connect("not a url").await.unwrap_err();
        assert!(matches!(err, RedisError::Connection(_)));
        assert!(matches!(
            OrmError::from(err),
            OrmError::Backend(BackendError::Connection(_))
        ));
    }
}
