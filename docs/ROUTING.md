# Routing

siderite routes are plain values. A path plus a `MethodRouter` makes a `Route`;
an `App` collects routes, mounts other apps and generates the OpenAPI 3.1
document from the same values, so docs cannot drift from the code.

## Function API

```rust
use siderite::prelude::*;

let app = App::new()
    .title("Users")
    .version("1.0.0")
    .route(
        "/users",
        post(create_user).status(StatusCode::CREATED).tag("users")
            .get(list_users).summary("List users"),
    );
```

* `get`, `post`, `put`, `patch`, `delete`, `head`, `options` create a
  `MethodRouter` and can be chained (`.get(..).post(..)`); `.on(Method, h)`
  takes any method.
* Metadata builders `.summary() .description() .tag() .operation_id()
  .deprecated() .hidden() .status(StatusCode) .response_model::<T>()` apply to
  the **most recently added** method, so order matters.
* `App::routes(iter)` accepts `Route`s (`Route::new(path, router)`), which is
  what the macros below produce.
* Path templates use `{name}` parameters (`{*rest}` for a catch-all). Duplicate
  `(path, method)` pairs and paths without a leading `/` are rejected when
  the app is built.

## Route macros

`#[get]`, `#[post]`, `#[put]`, `#[patch]`, `#[delete]`, `#[head]`,
`#[options]` and `#[ws]` (a `GET`) attach a route to an `async fn`.

```rust
/// Create a user.
///
/// Stores the user and returns it.
#[post("/users", status = 201, response_model = UserOut, tag = "users",
       tags("a", "b"), summary = "..", description = "..",
       operation_id = "..", deprecated, hidden)]
async fn create_user(Json(input): Json<UserIn>) -> Json<UserOut> { /* .. */ }
```

The first argument is the path (a string literal). All other arguments are
optional and may appear in any order; each may appear once (`tag` and `tags`
are separate keys and are combined).

### Exact expansion

The function is left untouched (it stays callable). Next to it, with the same
visibility, the macro emits:

```rust
#[doc(hidden)]
#[allow(non_snake_case, dead_code)]
fn __siderite_route_create_user() -> ::siderite::Route {
    ::siderite::Route::new(
        "/users",
        ::siderite::post(create_user)
            .operation_id("create_user")
            .summary("Create a user.")
            .description("Stores the user and returns it.")
            .tag("users").tag("a").tag("b")
            .status(::siderite::http::StatusCode::from_u16(201)
                .unwrap_or(::siderite::http::StatusCode::OK))
            .response_model::<UserOut>(),
    )
}
```

`deprecated` and `hidden` add `.deprecated()` / `.hidden()`. `#[ws]` uses
`::siderite::get`. The `unwrap_or` fallback is unreachable: the status is
validated at compile time.

### Metadata semantics

* `operation_id` defaults to the function name.
* `summary` defaults to the first line of the doc comment; `description`
  defaults to the remaining lines (each trimmed, joined with `\n`, omitted
  when empty). Explicit arguments replace the doc-derived value.
* `status` (100..=599) replaces a `200 OK` returned by the handler at runtime
  and moves the documented success response to that code. Handlers returning
  another status (e.g. `WithStatus`) are not altered.
* `response_model = T` documents the success body as `T` (`application/json`)
  regardless of the return type; it does not convert the response.
* `hidden` serves the route but omits it from the OpenAPI document.

### Compile-time diagnostics

Reported with spans on the offending token: path not a string literal or not
starting with `/`, unbalanced braces, invalid or duplicate `{param}` names,
unknown or duplicate arguments, status outside 100..=599, non-`async` or
generic handlers, and route attributes on items other than functions.

## `routes![]`

```rust
App::new().routes(routes![create_user, users::list])
```

expands to

```rust
::std::vec![__siderite_route_create_user(), users::__siderite_route_list()]
```

Only the last path segment is rewritten (a leading `::` is kept); generic
arguments are rejected. `routes![]` is an empty `Vec`; a trailing comma is
fine. Because the generated function is separate from the handler, refer to
handlers by a path where they are defined (`users::list`), not through a
`use users::list;` re-import of the handler alone.

## Handlers, extractors and responses

A handler is an `async fn` with zero or more arguments returning something
that implements `IntoResponse`. Every argument except the last implements
`FromRequestParts`; the **last** argument may implement `FromRequest` and thus
consume the request body (`Json<T>`, `Form<T>`, raw bodies). Built-in
extractors: `Path<T>`, `Query<T>`, `Json<T>`, `State<T>`, `Depends<T>`,
`HttpBearer` and the other security schemes (see [SECURITY.md](SECURITY.md)),
... All traits are siderite's own; no axum types leak into the public API.

Custom extractors and responses document themselves through the optional
`describe` hook, which receives the `Operation` being built and the
`SchemaRegistry`:

```rust
struct ApiKey;

impl FromRequestParts for ApiKey {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> { /* .. */ }

    fn describe(op: &mut Operation, r: &mut SchemaRegistry) {
        op.add_parameter(Parameter::new(
            "x-api-key", ParameterLocation::Header, true, r.subschema::<String>(),
        ));
    }
}
```

`IntoResponse::describe` works the same way (e.g. `op.add_response(..)`).
The default `describe` documents nothing, so an undocumented extractor
still works.

`Option<T>` makes an extractor optional. It is `None` only when `T` fails
with an error marked `ApiError::absent()` (a missing header, query string or
credential); any other error, such as a malformed value, still fails the
request. Custom extractors call `.absent()` on their "input missing" error to
opt in.

Types used in bodies, queries and responses derive `Schema` (see below);
nested types are always obtained through `registry.subschema::<T>()`, so named
types become `components.schemas` entries referenced by `$ref`.

## Server-sent events

`Sse::new(stream)` answers with a `text/event-stream` body from a stream of `Result<Event, E>`; `siderite::sse::Event` encodes one event:

```rust
use siderite::sse::{Event, Sse};

let event = Event::data("line one\nline two").event("update")?.id("7")?;
let keepalive = Event::comment("still here");
```

- `Event::data(text)` takes any text: each line (`\n`, `\r\n` or `\r` ends one) becomes its own `data:` line, so the client gets the text back with `\n` line ends. `Event::json(&value)` sends JSON. `.retry(duration)` sets the reconnection delay in whole milliseconds, and `Event::comment(text)` sends a comment line that clients ignore.
- `.event(name)` and `.id(id)` reject a CR or LF (and a NUL in an id, which clients ignore) with `SseError`, so a value can never split or forge an event.
- Responses carry `Cache-Control: no-store`, so caches never store or replay a stream, and the OpenAPI response is documented as `text/event-stream`.
- An `Err` from the stream ends the body abruptly and is never sent to the client, which sees the connection close and may reconnect (sending `Last-Event-ID`). Send an event first if the client should know why.

## Pagination

`Query<PageParams>` (`use siderite::{Page, PageParams};`; they are not in the prelude, so an application's own `Page` type does not clash) reads `?page=N&per_page=M`: a 1-based page (default 1) and a page size from 1 to 100 (default 20). Choose other bounds with `PageParams<MAX, DEFAULT>`, for example `Query<PageParams<500, 50>>`; `1 <= DEFAULT <= MAX` is checked at compile time. Zero, negative, non-numeric or too large values are answered with 422 before the handler runs, other query keys are left to other extractors, and the OpenAPI parameters state the minimum, maximum and defaults.

`Page<T>` is the response body: `items`, `total`, `page`, `per_page` and `total_pages`. `params.fetch(queryset)` runs `QuerySet::paginate` (unchanged: one `COUNT`, then one fetch with `OFFSET`/`LIMIT`) and returns a `Page<M>`; `page.map(f)` and `page.try_map(f)` convert the items (for example to response types), and `Page::from(orm_page)` adapts an ORM page. Its schema is inlined, not named, so pages of different item types never collide in `components.schemas`.

```rust
#[get("/items")]
async fn list_items(
    State(dbs): State<Databases>,
    Query(params): Query<PageParams>,
) -> ApiResult<Json<Page<ItemOut>>> {
    let rows = dbs.objects::<Item>()?.order_by([Item::id.asc()]);
    Ok(Json(params.fetch(rows).await?.map(ItemOut::from)))
}
```

The count and the fetch are two statements: rows written between them can make `total` disagree with the items, and an unordered queryset can show a row on two pages or on none. Order by a unique key, and run both inside a transaction with a snapshot (for example `REPEATABLE READ` on PostgreSQL) when the two must agree. A page past the end has no items and the real `total`.

## Mounting and OpenAPI merge

`App::mount(prefix, app)` (alias `nest`) serves `app` below `prefix`. For
OpenAPI, the child's operations are added to the parent's document under the
prefixed path (`/` maps to the prefix itself); component schemas from all
apps share one registry, so a type used in several places is emitted once.
Two different Rust types claiming the same schema name, duplicate operation
ids, and duplicate `(path, method)` pairs are configuration errors reported
when the app is built (`TestClient::try_new`, `run`) or by `App::openapi()`.
Startup/shutdown hooks of mounted apps are lifted to the root; title, version
and docs settings come from the root app only. State registered with
`with_state` applies to the routes of the app it was called on.

## `#[derive(Schema)]`

Generates `Schema` for structs and enums, honouring the common serde
attributes (`rename`, `rename_all`, `skip*`, `default`, `deny_unknown_fields`,
`tag`/`content`/`untagged`) plus `#[field(..)]` constraints (`min_length`,
`max_length`, `pattern`/`regex`, `gt`, `ge`, `lt`, `le`, `multiple_of`,
`email`, `url`, `title`, `description`, `examples(..)`, `alias`). Unknown
`#[field]` keys are ignored so other derives can share the attribute. Doc
comments become descriptions. Constraints added to a `$ref` property wrap it as
`{"allOf": [{"$ref": ..}], ...}`.

* Non-generic types are named after the type; generic types are inline unless
  `#[schema(name = "..")]`; `#[schema(inline)]` always inlines.
* `flatten`, `transparent`, `from`/`into`/`try_from`, `remote`, `with` and
  `serialize_with`/`deserialize_with`, split `rename(serialize = ..)` forms,
  `other`, variant-level `untagged`, and tuple variants of internally tagged
  enums are rejected with a compile error rather than described wrongly.
* Internally tagged newtype variants are described as
  `allOf [tag object, inner]`, which assumes the inner type serializes as an
  object (as serde requires).

## Differences from FastAPI

* Routes are values; decorators only generate a sibling function, so nothing
  is registered at import time and handlers stay plain callable functions.
* Request validation is driven by types (`Json<T>`, `Query<T>`), not by
  default-argument markers. `#[field(..)]` constraints feed both the OpenAPI
  schema and `#[derive(Validate)]`.
* Dependency injection is explicit: `Depends<T>`, `Provided<T>` and `State<T>`
  (see [DEPENDENCY_INJECTION.md](DEPENDENCY_INJECTION.md)).
* `status` is per operation and replaces only a `200`, so handlers can still
  return specific statuses themselves.
* Routers are `App`s (`mount`) rather than a separate `APIRouter` type.
