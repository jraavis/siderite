//! `/books`: list, create, fetch, replace, update and delete `Book` rows.
//!
//! Example handlers without authentication: add your own checks before
//! exposing them.

use crate::models::book::Book;
use siderite::orm::Databases;
use siderite::prelude::*;
use siderite::{Page, PageParams};

/// Body of `POST /books` and `PUT /books/{id}`; invalid input is
/// answered with 422.
#[derive(Debug, Deserialize, Validate, Schema)]
#[serde(deny_unknown_fields)]
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

/// Body of `PATCH /books/{id}`: only the fields present change.
#[derive(Debug, Deserialize, Validate, Schema)]
#[serde(deny_unknown_fields)]
pub struct BookPatch {
    /// New `title`, when present; `null` is rejected.
    #[field(min_length = 1)]
    #[serde(default)]
    pub title: Change<String>,
    /// New `pages`, when present; `null` clears it.
    #[serde(default)]
    pub pages: Change<Option<i32>>,
    /// New `views`, when present; `null` is rejected.
    #[serde(default)]
    pub views: Change<i64>,
    /// New `price`, when present; `null` clears it.
    #[serde(default)]
    pub price: Change<Option<f64>>,
    /// New `in_stock`, when present; `null` is rejected.
    #[serde(default)]
    pub in_stock: Change<bool>,
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

/// List `Book` rows by `id`, one page at a time: a count and one fetch,
/// whatever the page size. Rows written between the two can make
/// `total` disagree with the items.
#[get("/books", tag = "book", operation_id = "list_book")]
pub async fn list_book(
    State(dbs): State<Databases>,
    Query(params): Query<PageParams>,
) -> ApiResult<Json<Page<BookOut>>> {
    let rows = dbs.objects::<Book>()?.order_by([Book::id.asc()]);
    Ok(Json(params.fetch(rows).await?.map(BookOut::from)))
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

/// Replace every field of a `Book`; 404 when there is none with this
/// `id`. The `id` itself never changes.
#[put("/books/{id}", tag = "book", operation_id = "replace_book")]
pub async fn replace_book(
    State(dbs): State<Databases>,
    Path(id): Path<i64>,
    Json(body): Json<NewBook>,
) -> ApiResult<Json<BookOut>> {
    let db = dbs.for_write::<Book>()?;
    let found = Book::objects(db).get(Book::id.eq(id)).await?;
    let NewBook {
        title,
        pages,
        views,
        price,
        in_stock,
    } = body;
    let mut row = Book {
        id: found.id,
        title,
        pages,
        views,
        price,
        in_stock,
    };
    row.save(db).await?;
    Ok(Json(row.into()))
}

/// Change the fields present in the body; 404 when there is no `Book`
/// with this `id`.
#[patch("/books/{id}", tag = "book", operation_id = "update_book")]
pub async fn update_book(
    State(dbs): State<Databases>,
    Path(id): Path<i64>,
    Json(body): Json<BookPatch>,
) -> ApiResult<Json<BookOut>> {
    let db = dbs.for_write::<Book>()?;
    let mut row = Book::objects(db).get(Book::id.eq(id)).await?;
    body.title.apply(&mut row.title);
    body.pages.apply(&mut row.pages);
    body.views.apply(&mut row.views);
    body.price.apply(&mut row.price);
    body.in_stock.apply(&mut row.in_stock);
    row.save(db).await?;
    Ok(Json(row.into()))
}

/// Delete a `Book`; 404 when there is none with this `id`.
#[delete(
    "/books/{id}",
    status = 204,
    tag = "book",
    operation_id = "delete_book"
)]
pub async fn delete_book(
    State(dbs): State<Databases>,
    Path(id): Path<i64>,
) -> ApiResult<NoContent> {
    let db = dbs.for_write::<Book>()?;
    let rows = Book::objects(db).filter(Book::id.eq(id));
    if rows.delete().await? == 0 {
        return Err(ApiError::not_found("Resource not found."));
    }
    Ok(NoContent)
}

/// The routes of this module, for `App::routes`.
pub fn routes() -> Vec<Route> {
    routes![
        list_book,
        create_book,
        get_book,
        replace_book,
        update_book,
        delete_book
    ]
    .into_iter()
    .collect()
}
