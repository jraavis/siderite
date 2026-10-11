//! Layered configuration builder.

use crate::error::ConfigError;
use crate::explain::ConfigEntry;
use crate::settings::{Settings, default_values};
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use figment::value::UncasedStr;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};

/// Optional TOML file consulted by [`crate::load`].
pub const DEFAULT_CONFIG_FILE: &str = "siderite.toml";

/// Environment prefix used by [`crate::load`] (`SIDERITE_APP__NAME`, …).
pub const DEFAULT_ENV_PREFIX: &str = "SIDERITE_";

/// Builds [`Settings`] from defaults, files, environment variables, and
/// programmatic overrides.
///
/// Precedence, lowest to highest: defaults, TOML file, environment (including
/// `DATABASE_URL` / `ADDR` aliases when [`Self::env_prefix`] is used),
/// [`Self::set`]. Overrides win regardless of call order; files and the
/// environment merge in the order they are added.
///
/// Nested environment keys use `__` as the separator after the prefix is
/// stripped: `SIDERITE_DATABASES__DEFAULT__URL` maps to
/// `databases.default.url`.
pub struct ConfigBuilder {
    figment: Figment,
    /// Programmatic overrides, merged last at extraction time.
    overrides: Figment,
    required_files: Vec<PathBuf>,
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigBuilder {
    /// Defaults only: `app.name = "siderite"`, `server.addr = "127.0.0.1:8000"`,
    /// `log.level = "info"`, `cache.max_entries = 1024`, empty `databases`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            figment: Figment::new().merge(Serialized::defaults(default_values())),
            overrides: Figment::new(),
            required_files: Vec::new(),
        }
    }

    /// Merge a TOML file that must exist at extraction time.
    #[must_use]
    pub fn file(self, path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let mut required_files = self.required_files;
        required_files.push(path.clone());
        Self {
            figment: self.figment.merge(Toml::file(&path)),
            required_files,
            ..self
        }
    }

    /// Merge a TOML file; a missing file is ignored.
    #[must_use]
    pub fn file_optional(self, path: impl AsRef<Path>) -> Self {
        Self {
            figment: self.figment.merge(Toml::file(path.as_ref())),
            ..self
        }
    }

    /// Merge environment variables with `prefix`, nested on `__`.
    ///
    /// Also maps the unprefixed aliases `DATABASE_URL` → `databases.default.url`
    /// and `ADDR` → `server.addr`. Prefixed variables override those aliases.
    #[must_use]
    pub fn env_prefix(self, prefix: &str) -> Self {
        Self {
            figment: self
                .figment
                .merge(well_known_env())
                .merge(Env::prefixed(prefix).split("__")),
            ..self
        }
    }

    /// Override a dotted key (`"server.addr"`, `"app.debug"`, …). Overrides
    /// take precedence over every file and environment source, whenever they
    /// were added; among overrides, later [`Self::set`] calls win.
    #[must_use]
    pub fn set(self, key: &str, value: impl Serialize) -> Self {
        Self {
            overrides: self.overrides.merge(Serialized::default(key, value)),
            ..self
        }
    }

    /// Deserialize the merged configuration into `T`.
    ///
    /// # Errors
    /// Returns [`ConfigError::MissingFile`] if a required file is absent, or
    /// [`ConfigError::Extract`] if the merged tree cannot be deserialized.
    /// Extract messages never include secret values.
    pub fn extract<T: DeserializeOwned>(&self) -> Result<T, ConfigError> {
        self.ensure_required_files()?;
        self.figment
            .clone()
            .merge(self.overrides.clone())
            .extract()
            .map_err(ConfigError::from)
    }

    /// Deserialize the merged configuration as [`Settings`].
    ///
    /// # Errors
    /// Same as [`Self::extract`].
    pub fn build(&self) -> Result<Settings, ConfigError> {
        self.extract()
    }

    /// Every resolved value with the source that set it (a default, a
    /// file, an environment variable or an override), redacted, sorted by
    /// key. It reads the same merged sources as [`Self::build`].
    ///
    /// # Errors
    /// [`ConfigError::MissingFile`] for a missing required file.
    pub fn explain(&self) -> Result<Vec<ConfigEntry>, ConfigError> {
        self.ensure_required_files()?;
        let merged = self.figment.clone().merge(self.overrides.clone());
        Ok(crate::explain::entries(&merged, &self.overrides))
    }

    fn ensure_required_files(&self) -> Result<(), ConfigError> {
        for path in &self.required_files {
            if !path.exists() {
                return Err(ConfigError::MissingFile(path.clone()));
            }
        }
        Ok(())
    }
}

/// Unprefixed environment variables and the keys they set.
pub(crate) const ALIASES: [(&str, &str); 2] = [
    ("DATABASE_URL", "databases.default.url"),
    ("ADDR", "server.addr"),
];

fn well_known_env() -> Env {
    Env::raw().filter_map(|key: &UncasedStr| {
        let alias = ALIASES.iter().find(|(var, _)| key == *var);
        alias.map(|(_, path)| (*path).into())
    })
}

/// Load [`Settings`] from optional `siderite.toml`, `SIDERITE_` environment
/// variables, and the `DATABASE_URL` / `ADDR` aliases.
///
/// # Errors
/// Returns [`ConfigError::Extract`] when the merged configuration cannot be
/// deserialized.
pub fn load() -> Result<Settings, ConfigError> {
    ConfigBuilder::new()
        .file_optional(DEFAULT_CONFIG_FILE)
        .env_prefix(DEFAULT_ENV_PREFIX)
        .build()
}
