//! The `Book` model (table `books`).

use siderite::prelude::*;

/// A row of `books`.
#[derive(Debug, Clone, Model)]
#[model(table = "books", ordering = ["id"])]
pub struct Book {
    /// Database key, assigned on insert.
    #[field(primary_key, auto)]
    pub id: i64,
    /// The `title` column.
    pub title: String,
    /// The `pages` column; `None` is `NULL`.
    pub pages: Option<i32>,
    /// The `views` column.
    pub views: i64,
    /// The `price` column; `None` is `NULL`.
    pub price: Option<f64>,
    /// The `in_stock` column.
    pub in_stock: bool,
}
