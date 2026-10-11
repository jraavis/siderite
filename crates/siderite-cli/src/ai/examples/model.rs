use siderite::prelude::*;

/// A product row; `makemigrations` turns changes into migrations.
#[derive(Debug, Clone, Model, Serialize, Deserialize, Validate, Schema)]
#[model(table = "products", ordering = ["id"])]
pub struct Product {
    /// Database key, assigned on insert.
    #[field(primary_key, auto)]
    #[serde(default)]
    pub id: i64,
    /// Display name.
    #[field(min_length = 1, max_length = 100)]
    pub name: String,
    /// Whether it can be ordered.
    pub in_stock: bool,
}

/// List the products in stock with one query.
#[get("/products")]
pub async fn list_products(db: Provided<Db>) -> ApiResult<Json<Vec<Product>>> {
    let query = Product::objects(&db).filter(Product::in_stock.eq(true));
    Ok(Json(query.all().await?))
}
