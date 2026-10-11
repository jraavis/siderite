# Cursor pagination: semantics (F02)

Status: design for review. Nothing here is implemented; the executable
contract is `crates/siderite-orm/tests/cursor_contract.rs` with its
fixtures in `cursor_contract.json`, checked against a reference model so a
future implementation can be held to the same cases.

Page-number pagination (`QuerySet::paginate`, `PageParams`/`Page`) stays
as it is. Cursor (keyset) pagination is for long or fast-changing lists
where `OFFSET` gets slow and rows shift between pages.

## Shape

- ORM: `QuerySet::after(cursor).limit(n)` fetching one page, and
  `QuerySet::before(cursor)` for the previous page. The result carries the
  items plus `next` and `previous` cursors (`None` at either end).
- HTTP: `?after=TOKEN&limit=N` or `?before=TOKEN&limit=N` (never both),
  answered with `{items, next, previous}`. There is no `total`; a client
  that needs one asks a separate count endpoint.

## Ordering

1. The ordering must be total. The queryset's `order_by` terms are used
   in order, and the primary key is appended as the last term (ascending)
   unless it is already present, so every row has a distinct position.
2. Each term has a direction. Mixed directions are allowed; the seek
   predicate is the expanded form (below), not a row-value comparison, so
   every backend gets the same semantics.
3. Ordering terms must be model columns (no expressions, aggregates or
   annotations): their values are what the cursor stores, and they must
   be readable from the fetched row.
4. Without an explicit ordering the model's `ordering` applies; without
   either, the primary key alone.

## Nulls

Nullable ordering columns are allowed. siderite fixes the null position
instead of inheriting the backend's (PostgreSQL sorts nulls last in
ascending order, MySQL and SQLite first): **nulls sort last in ascending
order and first in descending order**, as if null were larger than every
value. The ORDER BY is emitted with that position on every backend
(`NULLS LAST` / `NULLS FIRST`, or an `IS NULL` sort key where the syntax is
missing).

## The seek predicate

For terms `t1..tn` (the last is the primary key) and the cursor's values
`v1..vn`, the rows after the cursor are those where, for some `k`, the
first `k - 1` terms equal the cursor's and term `k` is past it:

```
(t1 > v1) OR (t1 = v1 AND t2 > v2) OR ... OR (t1 = v1 AND ... AND tn > vn)
```

with `>` replaced by `<` for a descending term. With nulls placed as
above, "past" for an ascending term is `t > v OR t IS NULL` when `v` is not
null, and nothing when `v` is null (only equal rows can follow); "equal"
is `t IS NULL` when `v` is null. Descending terms mirror this. `before`
uses the inverted directions, fetches, then reverses the page.

## Filter binding

The cursor holds the ordering values of the last row of the page and a
fingerprint of the queryset: a 64-bit hash of its filters (with their
bound values), ordering and model table. A cursor is accepted only by a
queryset with the same fingerprint, so a token from one list (or from the
same list with other filters) is rejected rather than seeking into a
different result set. Filters are not stored in the token: the client
repeats its query parameters, and the server rebuilds the same queryset.

## Token format, validation and versioning

- A token is unpadded base64url of: version byte (`1`), fingerprint (8
  bytes), direction byte (`a`fter or `b`efore), then one typed value per
  ordering term (type tag plus value: null, i64, f64, text, bool, bytes,
  timestamp, date, uuid, decimal-as-text).
- Tokens are opaque to clients but not secret: they are validated, not
  trusted. Rejected with `400` (and a stable error code) when: the token
  is not base64url, longer than 1 KiB, has an unknown version, a different
  fingerprint, the wrong number of values, or a value whose type does not
  match its ordering column. Signing (HMAC with an app secret) is an
  optional later addition for apps that want tamper evidence.
- Error codes: `cursor.E001` malformed (not base64url, too long,
  truncated), `cursor.E002` unknown version, `cursor.E003` fingerprint
  mismatch (another query), `cursor.E004` values that do not fit the
  ordering (count or types).
- A new token layout gets a new version byte. A server accepts the
  versions it can read; old tokens become `400` only when a version is
  retired, which is a documented breaking change.

## Backend capabilities

| Backend | Seek predicate | Null placement | Status |
|---|---|---|---|
| PostgreSQL | expanded OR | `NULLS FIRST/LAST` | supported |
| MySQL | expanded OR | `IS NULL` sort key | supported |
| SQLite | expanded OR | `NULLS FIRST/LAST` (3.30+) | supported |
| MongoDB | `$or` of the same terms | `null` sorts first: emulated with a computed sort key, or nullable terms rejected | to decide |
| Redis | none | none | capability error |

A backend without a feature fails with a capability error before any
query, like other unsupported plans.

## Query budget

One statement per page: the seek predicate plus `LIMIT n + 1`, the extra
row deciding whether a `next` (or `previous`) cursor exists and then being
dropped. No count. Related rows follow the queryset's
`select_related`/`prefetch_related` as for any fetch, so a page never runs
a query per row. An index on the ordering columns plus the primary key
makes each page a range scan.

## Consistency

Rows that exist for the whole iteration and whose ordering values do not
change are each returned exactly once, whatever is inserted or deleted
meanwhile. A row whose ordering values change during the iteration can be
skipped or returned twice. Rows inserted behind the cursor are not seen
by a forward iteration.
