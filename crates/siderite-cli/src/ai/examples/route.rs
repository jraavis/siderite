use siderite::prelude::*;

/// Body of `POST /notes`; invalid input is answered with 422.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewNote {
    /// Note text.
    #[field(min_length = 1, max_length = 500)]
    pub text: String,
}

/// A note in responses.
#[derive(Debug, Serialize, Schema)]
pub struct Note {
    /// Identifier.
    pub id: i64,
    /// Note text.
    pub text: String,
}

/// Fetch one note.
#[get("/notes/{id}", tag = "notes")]
pub async fn get_note(Path(id): Path<i64>) -> Result<Json<Note>, ApiError> {
    if id != 1 {
        return Err(ApiError::not_found("note not found"));
    }
    let text = "hello".to_owned();
    Ok(Json(Note { id, text }))
}

/// Create a note.
#[post("/notes", status = 201, tag = "notes")]
pub async fn create_note(Json(body): Json<NewNote>) -> Json<Note> {
    Json(Note {
        id: 1,
        text: body.text,
    })
}

/// The application factory: every route is registered explicitly.
pub fn app() -> App {
    App::new()
        .title("Notes")
        .version("0.1.0")
        .routes(routes![get_note, create_note])
}
