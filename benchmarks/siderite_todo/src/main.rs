//! Siderite Todo API benchmark binary matching FastAPI todo_app.

mod mongo_settings;
mod settings;

use siderite::prelude::*;
use siderite_backends::mongodb::MongoBackend;
use siderite_backends::mysql::MySqlBackend;
use siderite_backends::postgres::PgBackend;
use siderite_backends::sqlite::{GroupCommit, SqliteBackend};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

const SQLITE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS todos (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0
);
";

const PG_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS todos (
    id BIGSERIAL PRIMARY KEY,
    title VARCHAR(280) NOT NULL,
    done BOOLEAN NOT NULL DEFAULT FALSE
);
";

const MYSQL_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS todos (
    id BIGINT PRIMARY KEY AUTO_INCREMENT,
    title VARCHAR(280) NOT NULL,
    done BOOLEAN NOT NULL DEFAULT FALSE
);
";

#[derive(Debug, Clone, Model, Serialize, Deserialize, Validate, Schema)]
#[model(table = "todos", ordering = ["-id"])]
pub struct Todo {
    #[field(primary_key, auto)]
    #[serde(default)]
    pub id: i64,
    #[field(min_length = 1, max_length = 280)]
    pub title: String,
    pub done: bool,
}

#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewTodo {
    #[field(min_length = 1, max_length = 280)]
    pub title: String,
}

/// `DATABASE_POOL_SIZE` (the runner's `--pool-size`), ten by default like
/// `connect`. Applies to every SQL backend, with the same value in both apps.
fn pool_size() -> u32 {
    std::env::var("DATABASE_POOL_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10)
}

pub async fn open_db(url: &str) -> Result<Db, ApiError> {
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        let backend = PgBackend::connect_with(
            url,
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(pool_size())
                .min_connections(pool_size())
                .test_before_acquire(false),
        )
        .await
        .map_err(ApiError::internal)?;
        let db = Db::new(backend);
        db.execute_script(PG_SCHEMA)
            .await
            .map_err(ApiError::internal)?;
        Ok(db)
    } else if url.starts_with("mysql://") {
        open_mysql(url).await
    } else if url.starts_with("mongodb://") {
        let parsed = mongodb::options::ClientOptions::parse(url)
            .await
            .map_err(ApiError::internal)?;
        let name = parsed.default_database.as_deref().unwrap_or("siderite");
        let backend = MongoBackend::connect(url, name)
            .await
            .map_err(ApiError::internal)?;
        if std::env::var_os("BENCHMARK_RUNTIME").is_some() {
            let observed = mongo_settings::capture(backend.database(), &parsed).await?;
            settings::emit(observed);
        }
        Ok(Db::new(backend))
    } else {
        // `SQLITE_WAL=1` (the runner's `--sqlite-wal`) trades durability for
        // write throughput; the FastAPI app applies the same pragmas.
        let profile = std::env::var("SQLITE_PROFILE").unwrap_or_else(|_| {
            if std::env::var("SQLITE_WAL").is_ok_and(|v| v == "1") {
                "wal-normal".to_owned()
            } else {
                "default".to_owned()
            }
        });
        let backend = if profile == "wal-normal" || profile == "wal-full" {
            let options: SqliteConnectOptions = url.parse().map_err(ApiError::internal)?;
            let sync = if profile == "wal-full" {
                SqliteSynchronous::Full
            } else {
                SqliteSynchronous::Normal
            };
            SqliteBackend::connect_with(
                options
                    .journal_mode(SqliteJournalMode::Wal)
                    .synchronous(sync),
                SqlitePoolOptions::new().max_connections(pool_size()),
            )
            .await
        } else if profile == "default" {
            let options: SqliteConnectOptions = url.parse().map_err(ApiError::internal)?;
            SqliteBackend::connect_with(
                options,
                SqlitePoolOptions::new().max_connections(pool_size()),
            )
            .await
        } else {
            return Err(ApiError::internal("invalid SQLite profile"));
        }
        .map_err(ApiError::internal)?;
        // `SQLITE_GROUP_COMMIT=1` (the runner's `--siderite-group-commit`)
        // shares commits between concurrent inserts. FastAPI has no
        // counterpart, so results are labelled as a separate workload.
        let backend = if std::env::var("SQLITE_GROUP_COMMIT").is_ok_and(|v| v == "1") {
            backend.group_commit(GroupCommit::default())
        } else {
            backend
        };
        let db = Db::new(backend);
        db.execute_script(SQLITE_SCHEMA)
            .await
            .map_err(ApiError::internal)?;
        Ok(db)
    }
}

/// Select the explicitly requested driver with equally initialized pools.
///
/// Args:
///     url: MySQL benchmark database URL.
///
/// Returns:
///     Configured database, or an explicit driver/setup error.
async fn open_mysql(url: &str) -> Result<Db, ApiError> {
    let driver = std::env::var("SIDERITE_MYSQL_DRIVER").unwrap_or_else(|_| "sqlx".into());
    if driver == "native" {
        #[cfg(feature = "mysql-native")]
        {
            use siderite_backends::mysql::native::{NativeMySqlBackend, NativeMySqlOptions};
            let options = NativeMySqlOptions {
                max_connections: pool_size() as usize,
                ..NativeMySqlOptions::default()
            };
            let backend = NativeMySqlBackend::connect_with(url, options)
                .await
                .map_err(ApiError::internal)?;
            let db = Db::new(backend.clone());
            db.execute_script(MYSQL_SCHEMA)
                .await
                .map_err(ApiError::internal)?;
            backend.warm().await.map_err(ApiError::internal)?;
            return Ok(db);
        }
        #[cfg(not(feature = "mysql-native"))]
        return Err(ApiError::internal("build with mysql-native feature"));
    }
    if driver != "sqlx" {
        return Err(ApiError::internal("invalid SIDERITE_MYSQL_DRIVER"));
    }
    let options = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(pool_size())
        .min_connections(pool_size())
        .test_before_acquire(false);
    let backend = MySqlBackend::connect_with(url, options)
        .await
        .map_err(ApiError::internal)?;
    let db = Db::new(backend);
    db.execute_script(MYSQL_SCHEMA)
        .await
        .map_err(ApiError::internal)?;
    Ok(db)
}

#[get("/health")]
async fn health() -> PlainText<&'static str> {
    PlainText("ok")
}

#[get("/todos")]
async fn list_todos(db: Provided<Db>) -> Result<Json<Vec<Todo>>, ApiError> {
    let items = Todo::objects(&db).limit(20).all().await?;
    Ok(Json(items))
}

#[get("/todos/{id}")]
async fn get_todo(db: Provided<Db>, Path(id): Path<i64>) -> Result<Json<Todo>, ApiError> {
    let item = Todo::objects(&db).get(Todo::id.eq(id)).await?;
    Ok(Json(item))
}

#[post("/todos", status = 201)]
async fn create_todo(db: Provided<Db>, Json(body): Json<NewTodo>) -> Result<Json<Todo>, ApiError> {
    let todo = Todo::objects(&db)
        .create(Todo {
            id: 0,
            title: body.title,
            done: false,
        })
        .await?;
    Ok(Json(todo))
}

pub fn app(db: Db) -> App {
    App::new()
        .title("Todo Benchmark")
        .version("1.0.0")
        .provide(db)
        .routes(routes![health, list_todos, get_todo, create_todo])
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if std::env::var_os("BENCHMARK_RUNTIME").is_some() {
        let runtime = tokio::runtime::Handle::current();
        let workers = runtime.metrics().num_workers();
        eprintln!(
            "BENCHMARK_RUNTIME {}",
            serde_json::json!({
                "scheduler_workers": workers,
                "pid": std::process::id(),
            })
        );
    }
    let mut addr = "127.0.0.1:8081".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "run" {
            continue;
        }
        if arg == "--addr"
            && let Some(val) = args.next()
        {
            addr = val;
        }
    }

    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://todo_bench.db?mode=rwc".into());
    let db = match open_db(&url).await {
        Ok(db) => db,
        Err(err) => {
            eprintln!("Database error: {err}");
            return std::process::ExitCode::from(1);
        }
    };

    if std::env::var_os("BENCHMARK_RUNTIME").is_some() {
        match settings::capture(&db).await {
            Ok(Some(settings)) => settings::emit(settings),
            Ok(None) => {}
            Err(err) => {
                eprintln!("Settings capture failed: {err}");
                return std::process::ExitCode::from(1);
            }
        }
    }

    if let Err(err) = app(db).run(&addr).await {
        eprintln!("Server error: {err}");
        return std::process::ExitCode::from(1);
    }
    std::process::ExitCode::SUCCESS
}
