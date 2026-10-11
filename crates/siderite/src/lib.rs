//! siderite: a batteries-included, type-driven web framework.
//!
//! ```no_run
//! use siderite::prelude::*;
//!
//! async fn index() -> PlainText<&'static str> {
//!     PlainText("hello")
//! }
//!
//! # async fn run() -> Result<(), ServerError> {
//! App::new().route("/", get(index)).run("0.0.0.0:8000").await
//! # }
//! ```
#![forbid(unsafe_code)]

pub use siderite_cache as cache;
pub use siderite_config as config;
pub use siderite_core::*;
pub use siderite_macros::{
    Model, Schema, Validate, delete, get, head, model_hooks, options, patch, post, put, receiver,
    routes, ws,
};
pub use siderite_openapi as openapi;

/// The ORM, plus the column-type crates (`chrono`, `uuid`, `rust_decimal`) its
/// field types come from.
pub mod orm {
    pub use siderite_orm::*;
    pub use {chrono, rust_decimal, uuid};
}
pub use siderite_validation as validation;

/// Everything needed to write a typical application.
pub mod prelude {
    pub use crate::orm;
    pub use chrono::{DateTime, Utc};
    pub use serde::{Deserialize, Serialize};
    pub use siderite_core::{
        ApiError, ApiResult, App, BackgroundTaskError, BackgroundTaskLimits, BackgroundTasks,
        Cached, Cookies, Dependency, Depends, Form, FromRequest, FromRequestParts, Header, Html,
        IntoResponse, Json, Message, MethodRouter, NoContent, Path, PlainText, Provided, Query,
        Redirect, ResolveContext, Resource, Route, ServerError, State, WebSocket, WebSocketUpgrade,
        WithStatus, delete, get, head, options, patch, post, put,
    };
    pub use siderite_macros::{
        Model, Schema, Validate, delete, get, head, model_hooks, options, patch, post, put, routes,
        ws,
    };
    pub use siderite_orm::{
        Db, DbType, Expr, Field, ForeignKey, Model, ModelOps, OneToOne, OrmError, QuerySet, Related,
    };
    pub use siderite_validation::Change;
    pub use siderite_validation::{
        Schema, SchemaObject, SchemaRegistry, Validate, ValidationError, ValidationResult,
    };
}

/// Support code for macro expansions. Not part of the public API.
#[doc(hidden)]
pub mod __private;
