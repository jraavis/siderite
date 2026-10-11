# Testing

`siderite-testkit` drives an `App` in process, without sockets, and provides disposable databases. Add it as a dev-dependency.

## `TestClient`

```rust
use siderite_testkit::TestClient;

let client = TestClient::new(app());
let response = client.get("/users/1").await?;
assert_eq!(response.status, 200);
let user: User = response.json()?;

client.post_json("/users", &NewUser { name: "ann".into() }).await?;
```

`TestClient::new` panics on a misconfigured app (duplicate routes, bad paths, OpenAPI failures); `try_new` returns the error so a test can assert on it. Requests: `get`, `delete`, `post_json`, `post_raw(path, content_type, body)` and `send(Request<Body>)` for anything else (`siderite_testkit::http` is re-exported). A `TestResponse` has `status`, `headers`, `body`, `json::<T>()`, `text()` and `content_type()`.

`TestClient::start(app)` also runs the startup hooks, like `App::run` without a socket; call `shutdown()` to run the shutdown hooks.

## Overrides and databases: `TestClient::builder`

A built client owns an immutable service, so test-only configuration goes on the builder:

```rust
let client = TestClient::builder(app())
    .override_value(FakeMailer::default())              // replaces T::resolve with a clone
    .override_dependency::<CurrentUser, _, _>(|_head| async {
        Ok(CurrentUser::admin())                        // any request, no credentials
    })
    .with_database("default", test_db.db().clone())     // replaces the app's own alias
    .build();                                           // or try_build(), or start().await
```

`override_dependency` and `override_value` are the `App` methods described in [DEPENDENCY_INJECTION.md](DEPENDENCY_INJECTION.md). `with_database` calls `App::database`, so handlers see it through `State<Databases>` (see [DATABASE_ROUTING.md](DATABASE_ROUTING.md)).

## `TestDatabase`

```rust
use siderite_testkit::TestDatabase;

let db = TestDatabase::sqlite_memory()   // one shared in-memory connection
    .await?
    .with_models(&[Author::META, Post::META])   // tables from model metadata
    .await?;
```

| Method | Does |
|---|---|
| `sqlite_memory()` | a fresh in-memory SQLite database (feature `sqlite`, on by default) |
| `from_db(db)` | wraps any `Db`, for PostgreSQL or MySQL tests |
| `with_models(&[..])` | creates tables, indexes, constraints and join tables, as `makemigrations` plus `migrate` would |
| `with_migrations(dir)` | applies every migration file in `dir` |
| `with_signals(signals)` | attaches a signal registry (do it before cloning the handle out); the test counterpart of `AppCli::configure_db` |
| `db()`, `into_db()` | the `Db` |
| `isolated(f)` | runs `f(Db)` in a transaction that is **always rolled back** |

```rust
db.isolated(|tx| async move {
    Author { id: 0, name: "ann".into() }.save(&tx).await.unwrap();
    assert_eq!(Author::objects(&tx).count().await.unwrap(), 1);
}).await?;
// nothing was committed
assert_eq!(Author::objects(db.db()).count().await?, 0);
```

Rules for `isolated`:

* Use only the `Db` given to the closure. An in-memory SQLite database has one connection, and the transaction holds it, so querying the outer handle inside the closure waits forever.
* Do not keep clones of the transactional `Db` after the closure returns.
* A panic in the closure also discards the transaction.

Because every test rolls back, tests can share one database and still run independently. Tests that need commits (for example `on_commit` hooks) should use a fresh `sqlite_memory()` each.

## Live database tests

The suites for PostgreSQL, MySQL, MongoDB and Redis are gated: each test skips itself (and prints why) when its URL variable is unset or does not name the right scheme. [`docker-compose.yml`](../docker-compose.yml) at the repository root starts all four on offset host ports so they do not collide with local servers.

```bash
docker compose up -d --wait
DATABASE_URL=postgres://siderite:siderite@127.0.0.1:55432/siderite \
MYSQL_URL=mysql://root:siderite@127.0.0.1:53306/siderite \
MONGODB_URL='mongodb://127.0.0.1:57017/siderite?directConnection=true' \
REDIS_URL=redis://127.0.0.1:56379/15 \
cargo test --workspace --all-features -- --include-ignored
docker compose down
```

| Variable | Service (image) | Notes |
|---|---|---|
| `DATABASE_URL` | PostgreSQL (`postgres:17-alpine`) | must start with `postgres` |
| `MYSQL_URL` (or `DATABASE_URL`) | MySQL (`mysql:8.4`) | connect as **root**: the tests create one database per test |
| `MONGODB_URL` | MongoDB (`mongo:8`) | runs as a **single-node replica set** (`rs0`, initiated by the health check) because the tests use transactions |
| `REDIS_URL` | Redis (`redis:7-alpine`) | must select database 15 (the tests refuse anything else); they delete only their own key prefix |

The credentials in the compose file are for local testing only. See [BACKENDS.md](BACKENDS.md) for the live-test status of each backend.

Migration lock and crash tests (`crates/siderite-migrations/tests/lock_live.rs`, `crash_*_live.rs`) need `--test-threads=1`. Each case has a private database, but the MySQL migration lock (`GET_LOCK('siderite_migrate')`) is server-wide, not per database, so concurrent cases, or another migrator on the same server, would contend for it. The PostgreSQL advisory lock is per database.
