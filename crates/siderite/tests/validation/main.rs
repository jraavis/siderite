//! `derive(Validate)`, `derive(Schema)` dumping and `#[model_hooks]` end to
//! end: through HTTP, differentially against Serde, and in the OpenAPI
//! document.
#![allow(clippy::unwrap_used)]
#![allow(dead_code)]

mod differential;
mod edge;
mod fixtures;
mod hooks;
mod http_validation;
mod openapi;
mod serialization;
