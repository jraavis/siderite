---
title: Extractors and responses
description: Path, Query, Json, Form, headers, cookies, WebSockets, files, and custom describe hooks.
---

A handler is an `async fn` with zero or more arguments returning something
that implements `IntoResponse`. Every argument except the last implements
`FromRequestParts`. The **last** argument may implement `FromRequest` and
consume the request body.

All of these traits are siderite’s own. No axum types leak into the public
API.

## Built-in extractors

| Extractor | Source | Notes |
|---|---|---|
| `Path<T>` | Path parameters | Struct → one parameter per field; tuple → positional; scalar → one parameter named from the template |
| `Query<T>` | Query string | One parameter per field of `T`. Repeated keys fill a `Vec` field. Text input always allows string-to-number coercion |
| `Json<T>` | JSON body | Runs the [validation](/siderite/guides/http/validation/) pipeline |
| `Form<T>` | `application/x-www-form-urlencoded` | Same validation pipeline; text input |
| `Multipart` | `multipart/form-data` | Streaming fields |
| `State<T>` | `App::with_state` | `Arc<T>` |
| `Depends<T>` | DI | See [Dependency injection](/siderite/guides/http/di/) |
| `Provided<T>` | `App::provide` | Application-scoped value that is not a `Dependency` |
| `Header<H>` | Named header | `H: NamedHeader` |
| `UserAgent`, `Accept` | Common headers | |
| `Cookies` | Cookie header | |
| `HttpBearer` and other schemes | Credentials | See [Security](/siderite/guides/http/security/) |
| `WebSocketUpgrade` | WebSocket handshake | Pair with `#[ws]` |
| `RawRequest` | Whole request | Escape hatch |

## Built-in responses

| Type | Status | Body |
|---|---|---|
| `Json<T>` | 200 | JSON through `Dump` (not raw Serde) |
| `JsonDump<T>` | 200 | JSON with `DumpOptions` (`exclude_none`, field sets, …) |
| `PlainText<T>` | 200 | `text/plain` |
| `Html<T>` | 200 | `text/html` |
| `NoContent` | 204 | empty |
| `Redirect` | 3xx | Location |
| `WithStatus<R>` | chosen | wraps another response |
| `WithHeaders<R>` | inner | extra headers |
| `WithCookies<R>` | inner | `Set-Cookie` |
| `StreamingResponse` | 200 | streaming body |
| `FileResponse` | 200 | file / static |

`App::docs` serves Swagger UI and ReDoc; static files and single-page
apps are `App` helpers (see below).

## Static files and single-page apps

`App::static_files("/assets", "public")` serves a directory under a prefix (`"/"` makes it the fallback for every unmatched path); missing files are the 404 problem document. The mount is not in the OpenAPI document.

`App::spa("/app", "dist")` serves a single-page application: files as they are, and `dist/index.html` for browser navigations to any other path below the prefix, so client-side routes such as `/app/settings` load the app (`"/"` mounts it for the whole site). It stays out of the way of everything else:

- Explicit routes win, including routes under the prefix (`/app/api/...`).
- Only `GET` and `HEAD` requests whose `Accept` names `text/html` and whose last segment has no extension fall back. A missing asset (`/app/assets/gone.js`), a request for JSON and a missing `index.html` get the 404 problem document; other methods get `405` with `Allow: GET, HEAD`.
- Paths are resolved inside the canonical directory: `..` segments (also percent-encoded), backslashes and NUL are refused, and a file reached through a symbolic link must also lie inside the directory.
- The fallback `index.html` is sent with `Cache-Control: no-cache`, so a new deploy is picked up; files keep `ServeFile`'s conditional and range handling.

## Optional extractors

Wrap an extractor in `Option` to accept requests without that input.
`Option<T>` is `None` only when the input is **absent**; input that is
present but invalid still fails the request with `T`'s error:

| Extractor | `None` when | Still an error |
|---|---|---|
| `Header<H>` | the header is not sent | an undecodable value (`422`) |
| `Query<T>` | there is no query string and `T` rejects the empty input | any query string `T` rejects (`422`) |
| `HttpBearer`, `OAuth2PasswordBearer<S>` | no `Authorization` header, or another scheme | a malformed token (`401`) |
| `HttpBasic` | no `Authorization` header, or another scheme | undecodable credentials (`401`) |
| `ApiKey<S>` | the key is not sent | |
| `Security<T, S>` | its credentials are absent | `T::authenticate` fails (`401`) |

`State<T>`, `Provided<T>` and `Resource<T>` never become `None`: a missing
value is a server misconfiguration and stays an error.

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
- Lifecycle: events are pulled only as fast as the connection takes them, so a slow reader never makes the server buffer events (feed the stream from a bounded channel, never an unbounded one); a client that disconnects drops the stream and the work behind it. `.keep_alive(interval)` sends a comment after `interval` without events, so idle connections are not closed. `.until(readiness.stopping())` ends the stream cleanly when shutdown begins (`Readiness::stopping()` resolves at Draining), so a graceful shutdown does not wait for the timeout and force-close it:

  ```rust
  async fn feed(readiness: Readiness) -> Sse<Events> {
      Sse::new(events())
          .keep_alive(Duration::from_secs(15))
          .until(async move { readiness.stopping().await })
  }
  ```

- Middleware: `Timeout` covers producing the response, not its body, so it does not cut streams; `Compression` leaves `text/event-stream` uncompressed, so events are not held back by the encoder. Both are tested through the server.
- Proxies must pass events through unbuffered: with nginx set `proxy_buffering off;` for the route (or add the `X-Accel-Buffering: no` header) and do not compress `text/event-stream`; set read timeouts above the keep-alive interval. Browsers allow about six HTTP/1.1 connections per host, so serve event streams over HTTP/2 when a page opens several.

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

### Cursor pagination

`Query<CursorParams>` (`use siderite::{CursorPage, CursorParams};`) reads
`?after=TOKEN&limit=N` or `?before=TOKEN&limit=N`: a page size from 1 to 100
(default 20; `CursorParams<MAX, DEFAULT>` chooses other bounds) and at most
one of the two cursors. Both cursors together, or a bad limit, are answered
with 422 before the handler runs.

`CursorPage<T>` is the response body: `items`, `next` and `previous`
(`null` at either end) and no `total`. `params.fetch(queryset)` runs
`QuerySet::cursor_page`: one statement per page, no count. Give the
queryset the same filters on every request and a stable `order_by`; the
primary key is appended, so ties are safe. A token the server rejects (a
mangled one, one from another list or filter, one from an older siderite)
is answered with `400` and the problem `code` `cursor.E001` to
`cursor.E004`; clients restart from the first page. `map`, `try_map` and
`CursorPage::from(orm_page)` work as for `Page`; the schema is inlined.

```rust
#[get("/items")]
async fn list_items(
    State(dbs): State<Databases>,
    Query(params): Query<CursorParams>,
) -> ApiResult<Json<CursorPage<ItemOut>>> {
    let rows = dbs.objects::<Item>()?.order_by([Item::id.asc()]);
    Ok(Json(params.fetch(rows).await?.map(ItemOut::from)))
}
```

Prefer it over `Page` for long or fast-changing lists. See
[the ORM guide](/siderite/guides/data/querysets/#cursor-pagination) for the ordering, null and consistency rules.

The count and the fetch are two statements: rows written between them can make `total` disagree with the items, and an unordered queryset can show a row on two pages or on none. Order by a unique key, and run both inside a transaction with a snapshot (for example `REPEATABLE READ` on PostgreSQL) when the two must agree. A page past the end has no items and the real `total`.

## Custom extractors

Implement `FromRequestParts` (or `FromRequest`) and optionally `describe` so
the extractor documents itself:

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

`IntoResponse::describe` works the same way (`op.add_response(..)`). The
default `describe` documents nothing, so an undocumented extractor still
runs.

For `Option<YourExtractor>` to be `None` when the input is missing, mark
that error with `ApiError::absent()`. Unmarked errors propagate through the
`Option`:

```rust
let key = parts.headers.get("x-api-key").ok_or_else(|| {
    ApiError::new(StatusCode::UNAUTHORIZED, "Not authenticated.").absent()
})?;
```

Types used in bodies, queries, and responses derive `Schema`. Nested types
go through `registry.subschema::<T>()`, so named types become
`components.schemas` entries referenced by `$ref`.

## `#[derive(Schema)]`

Generates `Schema` for structs and enums. It honours common serde
attributes (`rename`, `rename_all`, `skip*`, `default`,
`deny_unknown_fields`, `tag` / `content` / `untagged`) plus `#[field(..)]`
constraints (`min_length`, `max_length`, `pattern` / `regex`, `gt`, `ge`,
`lt`, `le`, `multiple_of`, `email`, `url`, `title`, `description`,
`examples(..)`, `alias`). Doc comments become descriptions.

- Non-generic types are named after the type; generic types are inline
  unless `#[schema(name = "..")]`. `#[schema(inline)]` always inlines.
- Constraints added to a `$ref` property wrap it as
  `{"allOf": [{"$ref": ..}], ...}`.
- `flatten`, `transparent`, `from` / `into` / `try_from`, `remote`,
  `with`, `serialize_with` / `deserialize_with`, split
  `rename(serialize = ..)` forms, `other`, variant-level `untagged`, and
  tuple variants of internally tagged enums are compile errors rather than
  described wrongly.

## See also

- [Routing](/siderite/guides/http/routing/)
- [Validation](/siderite/guides/http/validation/)
- [OpenAPI 3.1](/siderite/guides/http/openapi/)
