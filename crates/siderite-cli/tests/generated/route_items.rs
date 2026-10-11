//! The `/items` routes.

use siderite::prelude::*;

/// What `GET /items` returns.
#[derive(Debug, Serialize, Schema)]
pub struct ItemsInfo {
    /// What this resource is.
    pub message: String,
}

/// Describe the `items` resource.
#[get("/items", tag = "items", operation_id = "items_index")]
pub async fn items_index() -> Json<ItemsInfo> {
    Json(ItemsInfo {
        message: "items".to_owned(),
    })
}

/// The routes of this module, for `App::routes`.
pub fn routes() -> Vec<Route> {
    routes![items_index].into_iter().collect()
}
