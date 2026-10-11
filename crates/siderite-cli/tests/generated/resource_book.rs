//! `/books`: create and fetch `Book` rows.
//!
//! Example handlers without authentication: add your own checks before
//! exposing them.

use crate::models::book::Book;
use siderite::orm::Databases;
use siderite::prelude::*;

/// Body of `POST /books`; invalid input is answered with 422.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewBook {
    /// `title`; must not be empty.
    #[field(min_length = 1)]
    pub title: String,
    /// `pages`; may be omitted or `null`.
    #[serde(default)]
    pub pages: Option<i32>,
    /// `views`.
    pub views: i64,
    /// `price`; may be omitted or `null`.
    #[serde(default)]
    pub price: Option<f64>,
    /// `in_stock`.
    pub in_stock: bool,
}

/// A `Book` as returned to clients.
#[derive(Debug, Serialize, Schema)]
pub struct BookOut {
    /// Database key.
    pub id: i64,
    /// `title`.
    pub title: String,
    /// `pages`.
    pub pages: Option<i32>,
    /// `views`.
    pub views: i64,
    /// `price`.
    pub price: Option<f64>,
    /// `in_stock`.
    pub in_stock: bool,
}

impl From<Book> for BookOut {
    fn from(row: Book) -> Self {
        let Book {
            id,
            title,
            pages,
            views,
            price,
            in_stock,
        } = row;
        Self {
            id,
            title,
            pages,
            views,
            price,
            in_stock,
        }
    }
}

/// Create a `Book`; the response holds the stored row.
#[post("/books", status = 201, tag = "book", operation_id = "create_book")]
pub async fn create_book(
    State(dbs): State<Databases>,
    Json(body): Json<NewBook>,
) -> ApiResult<Json<BookOut>> {
    let NewBook {
        title,
        pages,
        views,
        price,
        in_stock,
    } = body;
    let mut row = Book {
        id: 0,
        title,
        pages,
        views,
        price,
        in_stock,
    };
    row.save(dbs.for_write::<Book>()?).await?;
    Ok(Json(row.into()))
}

/// Fetch one `Book`; 404 when there is none with this `id`.
#[get("/books/{id}", tag = "book", operation_id = "get_book")]
pub async fn get_book(
    State(dbs): State<Databases>,
    Path(id): Path<i64>,
) -> ApiResult<Json<BookOut>> {
    let found = dbs.objects::<Book>()?.get(Book::id.eq(id)).await?;
    Ok(Json(found.into()))
}

/// The routes of this module, for `App::routes`.
pub fn routes() -> Vec<Route> {
    routes![create_book, get_book].into_iter().collect()
}
