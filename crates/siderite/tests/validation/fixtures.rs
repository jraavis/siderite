//! Shared models, routes and helpers for the validation tests.

use serde_json::{Value, json};
use siderite::JsonDump;
use siderite::prelude::*;
use siderite::validation::{DumpOptions, FieldError, FieldSet};
use siderite_testkit::{TestClient, TestResponse};

// ---------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Address {
    #[field(pattern = r"^\d{5}$")]
    zip: String,
    city: String,
}

/// The reference model of `siderite_validation::model`, now derived.
#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[serde(rename_all = "camelCase")]
#[model_config(extra = "forbid", str_strip_whitespace, populate_by_name)]
pub struct Signup {
    #[field(min_length = 2, validation_alias = "login")]
    user_name: String,
    #[field(ge = 18)]
    age: Option<u8>,
    tags: Vec<String>,
    address: Address,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Lax {
    n: i32,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[model_config(strict = true)]
pub struct Strict {
    n: i32,
    lenient: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct FieldStrict {
    #[field(strict)]
    n: i32,
    m: i32,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[model_config(str_strip_whitespace, str_to_lower)]
pub struct Lower {
    name: String,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[model_config(str_to_upper)]
pub struct Upper {
    name: String,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Basket {
    items: Vec<Line>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Line {
    #[field(ge = 1, le = 10)]
    qty: u32,
    #[field(min_length = 1, max_length = 3)]
    code: String,
}

pub fn ten() -> u32 {
    10
}

#[derive(Debug, Deserialize, Validate, Schema)]
pub struct Search {
    q: String,
    #[serde(default = "ten")]
    #[field(default = 10, le = 100)]
    limit: u32,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    active: bool,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[serde(rename_all = "lowercase")]
pub enum Color {
    Red,
    #[serde(rename = "dark-green", alias = "green")]
    Green,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Palette {
    main: Color,
    accent: Option<Color>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Wrapper(#[field(min_length = 2)] pub String);

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Holder {
    wrapped: Wrapper,
}

pub fn positive_even(value: &i64) -> Result<(), FieldError> {
    if *value > 0 && value % 2 == 0 {
        Ok(())
    } else {
        Err(FieldError::new(
            "not_positive_even",
            "must be positive and even",
        ))
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Reusable {
    #[field(validator = positive_even)]
    a: i64,
    #[field(validator = positive_even)]
    b: i64,
}

// ---- hooks: ordering ------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Ordered {
    a: String,
    b: String,
    #[serde(default)]
    csv: Vec<String>,
}

#[model_hooks]
impl Ordered {
    #[model_validator(mode = "before")]
    fn poisoned(input: &mut Value) -> Result<(), FieldError> {
        if input.get("poison").is_some() {
            return Err(FieldError::new("poisoned", "poison key present"));
        }
        Ok(())
    }

    #[field_validator("csv", mode = "before")]
    fn split_csv(value: &mut Value) -> Result<(), FieldError> {
        if let Some(text) = value.as_str() {
            *value = Value::Array(text.split(',').map(|s| json!(s.trim())).collect());
        }
        Ok(())
    }

    #[field_validator("a")]
    fn a_long_enough(value: &str) -> Result<(), FieldError> {
        if value.len() < 3 {
            return Err(FieldError::new("a_short", "a is too short"));
        }
        Ok(())
    }

    #[field_validator("a", "b")]
    fn not_x(value: &str) -> Result<(), FieldError> {
        if value == "x" {
            return Err(FieldError::new("is_x", "must not be x"));
        }
        Ok(())
    }

    #[model_validator(mode = "after")]
    fn a_differs_from_b(&self) -> Result<(), FieldError> {
        if self.a == self.b {
            return Err(FieldError::new("same", "a and b must differ"));
        }
        Ok(())
    }

    #[model_validator]
    fn located(&self) -> Result<(), FieldError> {
        if self.csv.len() > 2 {
            return Err(FieldError::new("long", "too many").at("csv"));
        }
        Ok(())
    }
}

// ---- generics -------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Item {
    #[field(ge = 1)]
    qty: u32,
}

/// Generic models must opt in to hooks explicitly.
#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[model_config(hooks)]
pub struct Page<T> {
    items: Vec<T>,
    total: u32,
}

#[model_hooks]
impl<T> Page<T> {
    #[model_validator]
    fn total_matches(&self) -> Result<(), FieldError> {
        if usize::try_from(self.total).is_ok_and(|total| total == self.items.len()) {
            Ok(())
        } else {
            Err(FieldError::new(
                "total_mismatch",
                "total != number of items",
            ))
        }
    }

    #[computed_field]
    fn count(&self) -> u64 {
        u64::try_from(self.items.len()).unwrap_or(u64::MAX)
    }
}

// ---- serialization --------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Validate, Schema)]
#[serde(rename_all = "camelCase")]
pub struct User {
    first_name: String,
    last_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nickname: Option<String>,
    #[field(exclude)]
    pub password: String,
    #[serde(default)]
    #[field(default = 1)]
    level: u8,
    #[serde(default = "default_theme")]
    #[field(default_factory = default_theme)]
    theme: String,
    #[serde(default)]
    bio: Option<String>,
}

pub fn default_theme() -> String {
    "light".to_owned()
}

#[model_hooks]
impl User {
    /// Full display name.
    #[computed_field(alias = "fullName")]
    fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }

    #[computed_field]
    fn initials(&self) -> String {
        let first = self.first_name.chars().next().unwrap_or('?');
        let last = self.last_name.chars().next().unwrap_or('?');
        format!("{first}{last}")
    }

    #[field_serializer("last_name")]
    fn shout(value: &str) -> String {
        value.to_uppercase()
    }

    #[field_serializer("level")]
    fn level_as_value(value: &u8) -> Value {
        json!({ "value": value })
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Team {
    lead: User,
    members: Vec<User>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[serde(default)]
pub struct Config {
    name: String,
    retries: u8,
    verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "svc".to_owned(),
            retries: 3,
            verbose: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
pub struct Enveloped {
    id: u32,
}

#[model_hooks]
impl Enveloped {
    #[model_serializer]
    fn envelope(&self, value: Value) -> Value {
        json!({ "data": value, "version": 1 })
    }
}

// ---------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------

#[post("/signup")]
pub async fn signup(Json(s): Json<Signup>) -> Json<Signup> {
    Json(s)
}

#[post("/lax")]
pub async fn lax(Json(v): Json<Lax>) -> Json<Lax> {
    Json(v)
}

#[post("/strict")]
pub async fn strict(Json(v): Json<Strict>) -> Json<Strict> {
    Json(v)
}

#[post("/field_strict")]
pub async fn field_strict(Json(v): Json<FieldStrict>) -> Json<FieldStrict> {
    Json(v)
}

#[post("/lower")]
pub async fn lower(Json(v): Json<Lower>) -> Json<Lower> {
    Json(v)
}

#[post("/upper")]
pub async fn upper(Json(v): Json<Upper>) -> Json<Upper> {
    Json(v)
}

#[post("/basket")]
pub async fn basket(Json(v): Json<Basket>) -> Json<Basket> {
    Json(v)
}

#[get("/search")]
pub async fn search(Query(q): Query<Search>) -> Json<Value> {
    Json(json!({ "q": q.q, "limit": q.limit, "tags": q.tags, "active": q.active }))
}

#[post("/palette")]
pub async fn palette(Json(v): Json<Palette>) -> Json<Palette> {
    Json(v)
}

#[post("/holder")]
pub async fn holder(Json(v): Json<Holder>) -> Json<Holder> {
    Json(v)
}

#[post("/reusable")]
pub async fn reusable(Json(v): Json<Reusable>) -> Json<Reusable> {
    Json(v)
}

#[post("/ordered")]
pub async fn ordered(Json(v): Json<Ordered>) -> Json<Ordered> {
    Json(v)
}

#[post("/pages")]
pub async fn pages(Json(v): Json<Page<Item>>) -> Json<Page<Item>> {
    Json(v)
}

pub fn sample_user() -> User {
    User {
        first_name: "Ada".into(),
        last_name: "Lovelace".into(),
        nickname: None,
        password: "hunter2".into(),
        level: 1,
        theme: "light".into(),
        bio: None,
    }
}

#[get("/user")]
pub async fn user() -> Json<User> {
    Json(sample_user())
}

#[get("/users")]
pub async fn users() -> Json<Vec<User>> {
    Json(vec![sample_user(), sample_user()])
}

#[get("/team")]
pub async fn team() -> Json<Team> {
    Json(Team {
        lead: sample_user(),
        members: vec![sample_user()],
    })
}

#[get("/user/slim")]
pub async fn user_slim() -> JsonDump<User> {
    JsonDump(
        sample_user(),
        DumpOptions::new()
            .exclude_none()
            .exclude(FieldSet::of(["initials"])),
    )
}

#[get("/user/only")]
pub async fn user_only() -> JsonDump<User> {
    JsonDump(
        sample_user(),
        DumpOptions::new().include(FieldSet::of(["firstName", "fullName"])),
    )
}

#[get("/user/defaults")]
pub async fn user_defaults() -> JsonDump<User> {
    JsonDump(sample_user(), DumpOptions::new().exclude_defaults())
}

#[get("/config")]
pub async fn config() -> JsonDump<Config> {
    JsonDump(Config::default(), DumpOptions::new().exclude_defaults())
}

#[get("/enveloped")]
pub async fn enveloped() -> Json<Enveloped> {
    Json(Enveloped { id: 4 })
}

pub fn app() -> App {
    App::new().title("validation").version("1").routes(routes![
        signup,
        lax,
        strict,
        field_strict,
        lower,
        upper,
        basket,
        search,
        palette,
        holder,
        reusable,
        ordered,
        pages,
        user,
        users,
        team,
        user_slim,
        user_only,
        user_defaults,
        config,
        enveloped,
    ])
}

pub fn client() -> TestClient {
    TestClient::new(app())
}

/// `(joined location, code)` of every error in a 422 body.
pub fn errors(res: &TestResponse) -> Vec<(String, String)> {
    assert_eq!(res.status, 422, "{}", res.text());
    let body = res.json::<Value>().unwrap();
    body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let location: Vec<String> = e["location"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.to_string().trim_matches('"').to_owned())
                .collect();
            (location.join("."), e["code"].as_str().unwrap().to_owned())
        })
        .collect()
}

pub fn pair(location: &str, code: &str) -> (String, String) {
    (location.to_owned(), code.to_owned())
}

pub fn valid_signup() -> Value {
    json!({
        "userName": "bob",
        "age": 30,
        "tags": ["a"],
        "address": {"zip": "12345", "city": "X"},
    })
}
