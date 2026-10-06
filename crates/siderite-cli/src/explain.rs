//! `siderite explain CODE`: what a `check` issue id means and how to fix it.
//!
//! The catalog covers the stable ids [`mod@crate::check`] emits (`models.E001`,
//! `migrations.W001`, ...). Each entry gives the cause, the smallest valid
//! correction and the command that verifies it, for the installed framework
//! version. Tests keep the catalog, the ids in `src/check` and the id tables
//! of the CLI guides in sync.
//!
//! Compiler codes (`E0308`) and Clippy lints (`clippy::needless_return`) are
//! not explained here: the report points to `rustc --explain` or the Clippy
//! lint list instead. Any other code is unknown (exit 1).

use crate::args::{self, GlobalArgs};
use crate::envelope::{CliDiagnostic, CliEnvelope};
use crate::error::CliError;
use serde::Serialize;
use std::path::Path;

/// Format version of the explain report.
pub const EXPLAIN_FORMAT_VERSION: u32 = 1;

/// A guide section, looked up in the embedded docs index.
#[derive(Debug, Clone, Copy)]
struct DocRef {
    /// Path below `website/src/content/docs/`.
    page: &'static str,
    /// Section heading.
    heading: &'static str,
}

const fn doc(page: &'static str, heading: &'static str) -> DocRef {
    DocRef { page, heading }
}

const CHECK_DOC: DocRef = doc("guides/production/cli.md", "check");

/// One catalog entry.
#[derive(Debug, Clone, Copy)]
struct Entry {
    id: &'static str,
    level: &'static str,
    title: &'static str,
    cause: &'static str,
    fix: &'static str,
    verify: &'static str,
    since: &'static str,
    docs: &'static [DocRef],
}

const VERIFY_CHECK: &str = "siderite check";

/// Every id `check` emits. Keep in id order within each prefix.
const CATALOG: &[Entry] = &[
    Entry {
        id: "config.E001",
        level: "error",
        title: "models are registered but no `default` database is configured",
        cause: "The app registers models that migrations manage, but neither \
                `databases.default.url` nor `DATABASE_URL` is set, so there is no \
                database to hold them.",
        fix: "Set the URL in siderite.toml:\n\n    [databases.default]\n    \
              url = \"sqlite://app.db\"\n\nor export `DATABASE_URL`. Models that \
              migrations must not touch can use `#[model(managed = false)]`.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/production/config.md", "Settings"), CHECK_DOC],
    },
    Entry {
        id: "config.E002",
        level: "error",
        title: "a database URL has no scheme or does not parse",
        cause: "Emitted for two causes: the URL of `databases.<alias>.url` has no \
                `scheme:` prefix, or it has one but is not a valid URL (for \
                example an unescaped character in the password). The message \
                names the alias and never contains the URL.",
        fix: "Write a full URL such as `postgres://user:pass@localhost/app` or \
              `sqlite://app.db`; percent-encode reserved characters in the user \
              and password (`@` is `%40`).",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/production/config.md", "Settings"), CHECK_DOC],
    },
    Entry {
        id: "config.E003",
        level: "error",
        title: "a database URL uses an unsupported scheme",
        cause: "The URL scheme selects the backend, and it is not one of \
                sqlite, postgres, mysql, mongodb or redis.",
        fix: "Use a supported scheme (`postgresql://` is accepted for PostgreSQL) \
              and enable the matching cargo feature of `siderite`.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/backends.md", "Backends"), CHECK_DOC],
    },
    Entry {
        id: "models.E001",
        level: "error",
        title: "two models use the same table",
        cause: "Two registered models resolve to one table name. Without \
                `table = ...` the name is the snake_case of the type, so types \
                with the same name in different modules collide.",
        fix: "Give one model its own table: `#[model(table = \"blog_posts\")]`. \
              Renaming the table of an existing model needs a migration \
              (`siderite makemigrations`).",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/models.md", "#[model(...)]"), CHECK_DOC],
    },
    Entry {
        id: "models.E002",
        level: "error",
        title: "a model name is registered more than once",
        cause: "The same model appears more than once in the list passed to \
                `.models(...)`, or two types share a name.",
        fix: "Register each model once, e.g. `.models(&[User::META, Todo::META])`, \
              and rename one of two same-named types.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/models.md", "Models"), CHECK_DOC],
    },
    Entry {
        id: "models.E003",
        level: "error",
        title: "a model has no primary key",
        cause: "No field of the model has `#[field(primary_key)]`. Every model \
                needs exactly one.",
        fix: "Add a key field:\n\n    #[field(primary_key, auto)]\n    pub id: i64,\n\n\
              `auto` makes it database-generated (`i16`, `i32` or `i64`).",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[
            doc("guides/data/models.md", "#[field(...)] ORM keys"),
            CHECK_DOC,
        ],
    },
    Entry {
        id: "models.E004",
        level: "error",
        title: "a foreign key points to a model that is not registered",
        cause: "A `ForeignKey<T>` field targets a model `T` that is missing from \
                the app's `.models(...)` list, so its table is never created.",
        fix: "Register the target too: `.models(&[Post::META, Comment::META])`.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[
            doc("guides/data/relations.md", "Foreign keys and one-to-one"),
            CHECK_DOC,
        ],
    },
    Entry {
        id: "models.E005",
        level: "error",
        title: "a many-to-many relation (or its through model) targets an unregistered model",
        cause: "The target of `#[model(many_to_many(name(Target, ...)))]`, or \
                its `through = Model`, is not in the app's `.models(...)` list.",
        fix: "Register the target and any through model alongside the owning \
              model: `.models(&[Post::META, Tag::META])`.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/relations.md", "Many-to-many"), CHECK_DOC],
    },
    Entry {
        id: "models.E006",
        level: "error",
        title: "two fields map to the same column",
        cause: "Two fields of one model resolve to one column, e.g. a field \
                `author_id` next to `author: ForeignKey<User>` (whose column is \
                `author_id`), or a duplicate `column = \"...\"`.",
        fix: "Remove the duplicate field, or rename one column with \
              `#[field(column = \"other_name\")]`.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[
            doc("guides/data/models.md", "#[field(...)] ORM keys"),
            CHECK_DOC,
        ],
    },
    Entry {
        id: "models.E007",
        level: "error",
        title: "a model has more than one primary-key field",
        cause: "More than one field has `#[field(primary_key)]`; composite keys \
                are not supported.",
        fix: "Keep `primary_key` on one field and express the other uniqueness \
              with `#[model(unique_together([\"a\", \"b\"]))]`.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[
            doc("guides/data/models.md", "#[field(...)] ORM keys"),
            CHECK_DOC,
        ],
    },
    Entry {
        id: "migrations.E001",
        level: "error",
        title: "the migration files cannot be loaded",
        cause: "A file in the migrations directory is unreadable or is not a \
                valid migration JSON document.",
        fix: "Restore the file from version control, or fix the JSON the message \
              points to. Do not hand-edit migrations that were already applied.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/migrations.md", "File format"), CHECK_DOC],
    },
    Entry {
        id: "migrations.E002",
        level: "error",
        title: "the migration dependency graph is invalid",
        cause: "A migration depends on one that is not on disk, the dependencies \
                form a cycle, or there is more than one leaf (unmerged heads, \
                typically after merging branches).",
        fix: "Restore the missing migration file or break the cycle. For \
              multiple heads, add a merge migration file whose `dependencies` \
              list every head (see the file format), or squash the branches with \
              `siderite squashmigrations FROM TO`.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/migrations.md", "File format"), CHECK_DOC],
    },
    Entry {
        id: "migrations.E003",
        level: "error",
        title: "the migrations do not replay cleanly",
        cause: "Applying the migrations in order to an empty project state fails, \
                e.g. one alters a table or field an earlier one never created.",
        fix: "Fix the operation the message names in the latest unapplied \
              migration, or regenerate it with `siderite makemigrations`.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[
            doc("guides/data/migrations.md", "Operations and reversibility"),
            CHECK_DOC,
        ],
    },
    Entry {
        id: "migrations.E004",
        level: "error",
        title: "makemigrations would refuse the pending model changes",
        cause: "The model changes are ambiguous: a model or field was removed \
                and one with the same shape added, which could be a rename \
                (keeps data) or a drop (loses data).",
        fix: "`siderite makemigrations` has no rename option yet. To keep the \
              data, write the migration by hand with a `RenameModel` or \
              `RenameField` operation; or generate it from code with \
              `siderite_migrations::diff_with` and `RenameHints::rename_model` / \
              `rename_field` (`allow_drop_model` / `allow_drop_field` when the \
              drop is intended). Alternatively make the rename and any other \
              change in separate migrations.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[
            doc("guides/data/migrations.md", "Operations and reversibility"),
            CHECK_DOC,
        ],
    },
    Entry {
        id: "migrations.W001",
        level: "warning",
        title: "model changes are not recorded in any migration",
        cause: "The registered models differ from the state the migrations \
                build, so `migrate` would leave the database behind the code.",
        fix: "Record the changes: `siderite makemigrations --name describe_change`, \
              review the new file, then `siderite migrate`.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/migrations.md", "Migrations"), CHECK_DOC],
    },
    Entry {
        id: "openapi.E001",
        level: "error",
        title: "the OpenAPI document cannot be generated",
        cause: "Two routes produce the same operation (method and path) or the \
                same `operationId`. The id defaults to the handler function name.",
        fix: "Give one route a distinct id with `.operation_id(\"create_user\")`, \
              or remove the duplicate route.",
        verify: "siderite routes --json",
        since: "0.1.0",
        docs: &[doc("guides/http/routing.md", "Metadata"), CHECK_DOC],
    },
    Entry {
        id: "routes.E001",
        level: "error",
        title: "the app cannot be built into a router",
        cause: "Building the router failed: a method and path are registered \
                twice, or a path is malformed.",
        fix: "Remove or rename the duplicate route, or fix the path the message \
              names (paths start with `/`; parameters are `{name}`).",
        verify: "siderite routes",
        since: "0.1.0",
        docs: &[doc("guides/http/routing.md", "Routing"), CHECK_DOC],
    },
    Entry {
        id: "backend.E001",
        level: "error",
        title: "the `default` database is Redis, which cannot hold models",
        cause: "Models live on the `default` alias, and its URL is a redis:// \
                key/value store.",
        fix: "Point `databases.default.url` at sqlite, postgres, mysql or mongodb \
              and configure Redis under another alias (e.g. for the cache).",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/backends.md", "Redis"), CHECK_DOC],
    },
    Entry {
        id: "backend.E002",
        level: "error",
        title: "a model needs joins the `default` backend does not support, or the schema diff failed",
        cause: "Emitted for two causes: a model has foreign keys or many-to-many \
                relations and the backend (MongoDB) has no joins; or computing \
                the initial schema for the models failed.",
        fix: "For relations, store the related id as a plain field, or use a SQL \
              backend. For a failed diff, fix the model the message names.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/backends.md", "MongoDB"), CHECK_DOC],
    },
    Entry {
        id: "backend.E003",
        level: "error",
        title: "the schema cannot be created on the `default` backend",
        cause: "A model uses a column or constraint the backend's DDL cannot \
                express, e.g. a keyed TEXT column without `max_length` on MySQL.",
        fix: "Change the field the message names, e.g. add \
              `#[field(max_length = 255)]`, or choose a backend that supports it.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/migrations.md", "DDL"), CHECK_DOC],
    },
    Entry {
        id: "backend.W001",
        level: "warning",
        title: "the backend has no schema migrations",
        cause: "The `default` backend (MongoDB) is schemaless, so `migrate` \
                refuses to run.",
        fix: "Nothing to fix when the backend is intended; create collections \
              and indexes outside the migration system.",
        verify: VERIFY_CHECK,
        since: "0.1.0",
        docs: &[doc("guides/data/backends.md", "MongoDB"), CHECK_DOC],
    },
];

/// What kind of code was asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeKind {
    /// A `check` issue id explained by this catalog.
    Framework,
    /// A rustc error code such as `E0308`.
    Rustc,
    /// A Clippy lint such as `clippy::needless_return`.
    Clippy,
    /// A rustc lint such as `unused_variables`.
    RustcLint,
    /// Not recognized.
    Unknown,
}

/// A documentation link resolved from the embedded index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocLink {
    /// Section heading.
    pub heading: String,
    /// Repository path and line, `path:line`.
    pub source: String,
    /// Site URL.
    pub url: String,
}

/// Explanation of a framework code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Explanation {
    /// Level `check` reports it at: `error` or `warning`.
    pub level: &'static str,
    /// One-line meaning.
    pub title: &'static str,
    /// Why it is reported.
    pub cause: &'static str,
    /// Smallest valid correction.
    pub fix: &'static str,
    /// Command that shows the issue is gone.
    pub verify: &'static str,
    /// First framework version that emits it.
    pub since: &'static str,
    /// Related guide sections.
    pub docs: Vec<DocLink>,
}

/// The `explain` report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExplainReport {
    /// Report format version.
    pub format_version: u32,
    /// Framework version the catalog describes.
    pub framework_version: String,
    /// Normalized code.
    pub code: String,
    /// What kind of code it is.
    pub kind: CodeKind,
    /// Present for framework codes.
    pub explanation: Option<Explanation>,
    /// Where to read about non-framework codes.
    pub pointers: Vec<String>,
    /// Known codes sharing the prefix, for unknown framework-looking codes.
    pub similar: Vec<&'static str>,
    /// Project `Cargo.lock` comparison, as in `docs search`.
    pub version_check: crate::docs::VersionCheck,
}

/// Every code the catalog explains, in catalog order.
#[must_use]
pub fn known_codes() -> Vec<&'static str> {
    CATALOG.iter().map(|e| e.id).collect()
}

/// Strip `[` `]`, backticks and `:` that `check` text output or Markdown
/// wrap around a code, and case-fold the framework parts.
fn normalize(raw: &str) -> String {
    let code = raw
        .trim()
        .trim_matches(|c: char| matches!(c, '[' | ']' | '`' | ':' | ','));
    if let Some(lint) = code.strip_prefix("clippy::") {
        return format!("clippy::{}", lint.to_ascii_lowercase());
    }
    match code.split_once('.') {
        Some((prefix, rest)) => format!(
            "{}.{}",
            prefix.to_ascii_lowercase(),
            rest.to_ascii_uppercase()
        ),
        None if is_rustc_code(&code.to_ascii_uppercase()) => code.to_ascii_uppercase(),
        None => code.to_owned(),
    }
}

fn is_rustc_code(code: &str) -> bool {
    code.len() == 5 && code.starts_with('E') && code[1..].chars().all(|c| c.is_ascii_digit())
}

fn is_clippy_lint(code: &str) -> bool {
    code.strip_prefix("clippy::").is_some_and(|lint| {
        !lint.is_empty() && lint.chars().all(|c| c.is_ascii_lowercase() || c == '_')
    })
}

/// Bare snake_case names, the form rustc lint codes take in `verify --json`.
fn is_rustc_lint(code: &str) -> bool {
    code.contains('_')
        && code.starts_with(|c: char| c.is_ascii_lowercase())
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn resolve_docs(index: &crate::docs::DocIndex, refs: &[DocRef]) -> Result<Vec<DocLink>, CliError> {
    refs.iter()
        .map(|r| {
            index
                .sections
                .iter()
                .find(|s| {
                    s.heading == r.heading
                        && s.path
                            .strip_prefix(&format!("{}/", index.source))
                            .is_some_and(|p| p == r.page)
                })
                .map(|s| DocLink {
                    heading: s.heading.clone(),
                    source: format!("{}:{}", s.path, s.line),
                    url: s.url.clone(),
                })
                .ok_or_else(|| {
                    CliError::Io(format!(
                        "explain catalog links a missing guide section: {} › {}",
                        r.page, r.heading
                    ))
                })
        })
        .collect()
}

/// Build the report for `raw` (a code as typed or copied).
///
/// # Errors
/// When the embedded docs index is unreadable or lacks a linked section.
pub fn explain_report(start: &Path, raw: &str) -> Result<ExplainReport, CliError> {
    let index = crate::docs::embedded_index()?;
    let code = normalize(raw);
    let mut report = ExplainReport {
        format_version: EXPLAIN_FORMAT_VERSION,
        framework_version: env!("CARGO_PKG_VERSION").to_owned(),
        kind: CodeKind::Unknown,
        explanation: None,
        pointers: Vec::new(),
        similar: Vec::new(),
        version_check: crate::docs::version_check(start, env!("CARGO_PKG_VERSION")),
        code,
    };
    if let Some(entry) = CATALOG.iter().find(|e| e.id == report.code) {
        report.kind = CodeKind::Framework;
        report.explanation = Some(Explanation {
            level: entry.level,
            title: entry.title,
            cause: entry.cause,
            fix: entry.fix,
            verify: entry.verify,
            since: entry.since,
            docs: resolve_docs(&index, entry.docs)?,
        });
    } else if is_rustc_code(&report.code) {
        report.kind = CodeKind::Rustc;
        report.pointers = vec![
            format!("rustc --explain {}", report.code),
            format!("https://doc.rust-lang.org/error_codes/{}.html", report.code),
        ];
    } else if is_clippy_lint(&report.code) {
        report.kind = CodeKind::Clippy;
        let lint = &report.code["clippy::".len()..];
        report.pointers = vec![format!(
            "https://rust-lang.github.io/rust-clippy/master/index.html#{lint}"
        )];
    } else if is_rustc_lint(&report.code) {
        report.kind = CodeKind::RustcLint;
        report.pointers = vec![
            "rustc -W help".to_owned(),
            "https://doc.rust-lang.org/rustc/lints/listing/index.html".to_owned(),
        ];
    } else if let Some((prefix, _)) = report.code.split_once('.') {
        report.similar = CATALOG
            .iter()
            .filter(|e| e.id.split_once('.').is_some_and(|(p, _)| p == prefix))
            .map(|e| e.id)
            .collect();
    }
    Ok(report)
}

fn mismatch_message(report: &ExplainReport) -> Option<String> {
    let check = &report.version_check;
    (check.status == crate::docs::VersionStatus::Mismatch).then(|| {
        format!(
            "this explanation is for siderite {}, but the project locks {} {}",
            report.framework_version,
            check.package.as_deref().unwrap_or("siderite"),
            check.project_version.as_deref().unwrap_or("?")
        )
    })
}

fn unknown_message(report: &ExplainReport) -> String {
    let mut msg = format!(
        "`{}` is not a siderite diagnostic code, rustc error code or lint name",
        report.code
    );
    if report.similar.is_empty() {
        msg.push_str("; run `siderite explain --list` for the known codes");
    } else {
        msg.push_str(&format!("; known: {}", report.similar.join(", ")));
    }
    msg
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                "\n".to_owned()
            } else {
                format!("  {l}\n")
            }
        })
        .collect()
}

/// Text form of `report`.
#[must_use]
pub fn render(report: &ExplainReport) -> String {
    let mut out = String::new();
    if let Some(msg) = mismatch_message(report) {
        out.push_str(&format!("warning: {msg}\n\n"));
    }
    match report.kind {
        CodeKind::Framework => {
            let Some(e) = report.explanation.as_ref() else {
                return out;
            };
            out.push_str(&format!(
                "{} ({}, siderite {}; since {})\n{}\n\nCause:\n{}\nFix:\n{}\nVerify:\n  {}\n",
                report.code,
                e.level,
                report.framework_version,
                e.since,
                e.title,
                indent(e.cause),
                indent(e.fix),
                e.verify
            ));
            if !e.docs.is_empty() {
                out.push_str("\nDocs:\n");
                for d in &e.docs {
                    out.push_str(&format!("  {}  {}\n", d.url, d.source));
                }
            }
        }
        CodeKind::Rustc | CodeKind::Clippy | CodeKind::RustcLint => {
            let what = match report.kind {
                CodeKind::Rustc => "a Rust compiler error code",
                CodeKind::Clippy => "a Clippy lint",
                _ => "a rustc lint name",
            };
            out.push_str(&format!(
                "{} is {what}, not a siderite code. See:\n",
                report.code
            ));
            for p in &report.pointers {
                out.push_str(&format!("  {p}\n"));
            }
        }
        CodeKind::Unknown => out.push_str(&format!("{}\n", unknown_message(report))),
    }
    out
}

/// Text form of `--list`.
fn render_list() -> String {
    let mut out = format!(
        "siderite {} diagnostic codes (`siderite explain CODE`):\n",
        env!("CARGO_PKG_VERSION")
    );
    for e in CATALOG {
        out.push_str(&format!("  {:<16} {:<7} {}\n", e.id, e.level, e.title));
    }
    out
}

/// One `--list` row.
#[derive(Debug, Clone, Serialize)]
struct ListEntry {
    id: &'static str,
    level: &'static str,
    title: &'static str,
}

/// `--list` JSON data.
#[derive(Debug, Clone, Serialize)]
struct ListReport {
    format_version: u32,
    framework_version: &'static str,
    codes: Vec<ListEntry>,
}

const USAGE: &str = "usage: siderite explain CODE [--json] | siderite explain --list [--json]";

enum Request {
    List,
    Code(String),
}

fn parse(rest: &[String]) -> Result<Request, CliError> {
    let mut list = false;
    let mut positional = Vec::new();
    for arg in rest.iter().filter(|a| *a != "--json") {
        if arg == "--list" {
            list = true;
        } else if arg.starts_with("--") {
            return Err(CliError::usage(format!("unexpected flag `{arg}`; {USAGE}")));
        } else {
            positional.push(arg.as_str());
        }
    }
    let mut positional = positional.into_iter();
    if positional.next() != Some("explain") {
        return Err(CliError::usage(USAGE));
    }
    let codes: Vec<&str> = positional.collect();
    match (list, codes.as_slice()) {
        (true, []) => Ok(Request::List),
        (false, [code]) => Ok(Request::Code((*code).to_owned())),
        (false, []) => Err(CliError::usage(format!("missing CODE; {USAGE}"))),
        _ => Err(CliError::usage(format!("expected one CODE; {USAGE}"))),
    }
}

fn print_json<T: Serialize>(env: &CliEnvelope<T>, to_stderr: bool) -> Result<(), CliError> {
    let json = env
        .to_json_pretty()
        .map_err(|err| CliError::Io(format!("failed to serialize JSON: {err}")))?;
    if to_stderr {
        eprintln!("{json}");
    } else {
        println!("{json}");
    }
    Ok(())
}

/// `siderite explain CODE [--json]` and `siderite explain --list [--json]`.
///
/// Exit 0 when the code is explained or pointed to, 1 when it is unknown.
///
/// # Errors
/// Usage errors, or an unreadable embedded docs index.
pub fn run(cwd: &Path, global: &GlobalArgs, raw: &[String]) -> Result<u8, CliError> {
    let (_, rest) = args::split_global(raw)?;
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(0);
    }
    let code = match parse(&rest)? {
        Request::List => {
            if global.json {
                let data = ListReport {
                    format_version: EXPLAIN_FORMAT_VERSION,
                    framework_version: env!("CARGO_PKG_VERSION"),
                    codes: CATALOG
                        .iter()
                        .map(|e| ListEntry {
                            id: e.id,
                            level: e.level,
                            title: e.title,
                        })
                        .collect(),
                };
                print_json(&CliEnvelope::success("explain", data), false)?;
            } else {
                print!("{}", render_list());
            }
            return Ok(0);
        }
        Request::Code(code) => code,
    };
    let start = crate::docs::search_start(cwd, global)?;
    let report = explain_report(&start, &code)?;
    let unknown = report.kind == CodeKind::Unknown;
    if global.json {
        let mismatch = mismatch_message(&report);
        let unknown_msg = unknown.then(|| unknown_message(&report));
        let mut env = CliEnvelope::success("explain", report);
        if let Some(msg) = mismatch {
            env.diagnostics
                .push(CliDiagnostic::warning("DOCS_VERSION_MISMATCH", msg));
        }
        if let Some(msg) = unknown_msg {
            env.ok = false;
            env.diagnostics
                .push(CliDiagnostic::error("UNKNOWN_CODE", msg));
        }
        print_json(&env, false)?;
    } else if unknown {
        eprint!("{}", render(&report));
    } else {
        print!("{}", render(&report));
    }
    Ok(u8::from(unknown))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::collections::BTreeSet;

    /// Ids passed to `CheckIssue::error` / `warning` in `src/check`, outside
    /// `#[cfg(test)]` modules.
    fn emitted_ids() -> BTreeSet<String> {
        let sources = [
            include_str!("check/mod.rs"),
            include_str!("check/config.rs"),
            include_str!("check/models.rs"),
            include_str!("check/migrations.rs"),
            include_str!("check/backend.rs"),
            include_str!("check/openapi.rs"),
        ];
        let mut ids = BTreeSet::new();
        for src in sources {
            let src = src.split("#[cfg(test)]").next().unwrap_or(src);
            for chunk in src.split('"').skip(1).step_by(2) {
                let is_id = chunk.split_once('.').is_some_and(|(prefix, num)| {
                    !prefix.is_empty()
                        && prefix.chars().all(|c| c.is_ascii_lowercase())
                        && num.len() == 4
                        && (num.starts_with('E') || num.starts_with('W'))
                        && num[1..].chars().all(|c| c.is_ascii_digit())
                });
                if is_id {
                    ids.insert(chunk.to_owned());
                }
            }
        }
        ids
    }

    fn table_ids(guide: &str) -> BTreeSet<String> {
        let start = guide
            .find("| Id | Level | Meaning |")
            .expect("check id table");
        guide[start..]
            .lines()
            .skip(2)
            .take_while(|l| l.starts_with('|'))
            .map(|l| l.split('`').nth(1).expect("id in backticks").to_owned())
            .collect()
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("siderite-explain-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn catalog_ids() -> BTreeSet<String> {
        CATALOG.iter().map(|e| e.id.to_owned()).collect()
    }

    #[test]
    fn emitted_ids_scans_every_check_file() {
        let mut files: Vec<String> =
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src/check"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
        files.sort();
        assert_eq!(
            files,
            [
                "backend.rs",
                "config.rs",
                "migrations.rs",
                "mod.rs",
                "models.rs",
                "openapi.rs"
            ],
            "add new check files to emitted_ids"
        );
    }

    #[test]
    fn catalog_matches_emitted_ids() {
        assert_eq!(CATALOG.len(), catalog_ids().len(), "duplicate catalog id");
        assert_eq!(catalog_ids(), emitted_ids());
    }

    #[test]
    fn catalog_matches_guide_tables() {
        let cli = include_str!("../../../docs/CLI.md");
        let site = include_str!("../../../website/src/content/docs/guides/production/cli.md");
        assert_eq!(table_ids(cli), catalog_ids(), "docs/CLI.md");
        assert_eq!(table_ids(site), catalog_ids(), "website cli.md");
    }

    #[test]
    fn levels_match_ids_and_docs_resolve() {
        let index = crate::docs::embedded_index().unwrap();
        for e in CATALOG {
            let expected = if e.id.contains(".W") {
                "warning"
            } else {
                "error"
            };
            assert_eq!(e.level, expected, "{}", e.id);
            assert!(!e.docs.is_empty(), "{}", e.id);
            resolve_docs(&index, e.docs).unwrap();
            assert!(e.verify.starts_with("siderite "), "{}", e.id);
        }
    }

    #[test]
    fn normalizes_copied_codes() {
        assert_eq!(normalize("[models.E003]"), "models.E003");
        assert_eq!(normalize("MODELS.e003"), "models.E003");
        assert_eq!(normalize("`migrations.w001`"), "migrations.W001");
        assert_eq!(normalize("e0308"), "E0308");
        assert_eq!(normalize("bogus"), "bogus");
        assert_eq!(
            normalize("clippy::Needless_Return"),
            "clippy::needless_return"
        );
    }

    #[test]
    fn explains_framework_code() {
        let dir = temp_dir("t1");
        let r = explain_report(&dir, "[models.E003]").unwrap();
        assert_eq!(r.kind, CodeKind::Framework);
        let e = r.explanation.as_ref().unwrap();
        assert!(e.fix.contains("#[field(primary_key, auto)]"));
        assert_eq!(e.verify, "siderite check");
        assert!(
            e.docs
                .iter()
                .any(|d| d.url.ends_with("/guides/production/cli/#check"))
        );
        let text = render(&r);
        assert!(text.starts_with("models.E003 (error, siderite "));
        assert!(text.contains("Verify:\n  siderite check\n"));
    }

    #[test]
    fn points_to_compiler_and_clippy_sources() {
        let dir = temp_dir("t2");
        let r = explain_report(&dir, "E0308").unwrap();
        assert_eq!(r.kind, CodeKind::Rustc);
        assert_eq!(r.pointers[0], "rustc --explain E0308");
        assert!(r.explanation.is_none());
        let r = explain_report(&dir, "clippy::needless_return").unwrap();
        assert_eq!(r.kind, CodeKind::Clippy);
        assert!(r.pointers[0].ends_with("#needless_return"));
        let r = explain_report(&dir, "unused_variables").unwrap();
        assert_eq!(r.kind, CodeKind::RustcLint);
        assert_eq!(r.pointers[0], "rustc -W help");
    }

    #[test]
    fn unknown_codes_list_known_siblings() {
        let dir = temp_dir("t3");
        let r = explain_report(&dir, "models.E999").unwrap();
        assert_eq!(r.kind, CodeKind::Unknown);
        assert!(r.similar.contains(&"models.E001"));
        assert!(unknown_message(&r).contains("known: models.E001"));
        let r = explain_report(&dir, "bogus").unwrap();
        assert!(r.similar.is_empty());
        assert!(unknown_message(&r).contains("--list"));
        // Not rustc codes: wrong length, or not E-prefixed.
        assert!(!is_rustc_code("E030"));
        assert!(!is_rustc_code("W0308"));
    }

    #[test]
    fn parses_arguments() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(matches!(
            parse(&a(&["explain", "--list"])),
            Ok(Request::List)
        ));
        assert!(
            matches!(parse(&a(&["explain", "models.E001", "--json"])), Ok(Request::Code(c)) if c == "models.E001")
        );
        assert!(parse(&a(&["explain"])).is_err());
        assert!(parse(&a(&["explain", "a", "b"])).is_err());
        assert!(parse(&a(&["explain", "--list", "a"])).is_err());
        assert!(parse(&a(&["explain", "--bogus", "a"])).is_err());
    }

    #[test]
    fn warns_on_locked_version_mismatch() {
        let dir = temp_dir("t4");
        std::fs::write(
            dir.join("Cargo.lock"),
            "[[package]]\nname = \"siderite\"\nversion = \"0.0.1-old\"\n",
        )
        .unwrap();
        let r = explain_report(&dir, "models.E001").unwrap();
        assert!(render(&r).starts_with("warning: this explanation is for siderite"));
    }
}
