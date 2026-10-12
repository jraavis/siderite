//! The command contract: one table describing every `siderite` command.
//!
//! Dispatch, help text and `siderite commands --json` all read
//! [`COMMANDS`], so what a command needs (a package, the compiled app),
//! what it may change (project files, the database, build output),
//! whether it touches live services and which output formats it offers
//! are declared once. The JSON list is versioned for AI clients.

use serde::Serialize;

/// Version of the `commands` report, bumped for breaking changes.
pub const COMMANDS_VERSION: u32 = 1;

/// Where a command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Runner {
    /// In the `siderite` binary itself.
    Tool,
    /// In the application binary (`AppCli`), through `cargo run`.
    App,
    /// In the application binary when there is a package, else in the
    /// standalone binary on JSON migration files.
    AppOrStandalone,
    /// As a Cargo subcommand in the package; arguments after the command
    /// are forwarded unchanged.
    Cargo {
        /// The Cargo subcommand.
        cargo: &'static str,
        /// Whether `--bin` applies.
        takes_bin: bool,
        /// The rustup component providing the subcommand, if any.
        component: Option<&'static str>,
    },
}

/// What a command needs to find.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Needs {
    /// Nothing; it works anywhere.
    Nothing,
    /// A Cargo package.
    Package,
    /// An application package (one that depends on `siderite`).
    App,
}

/// What a command may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mutation {
    /// Files in the project (never overwriting user content).
    ProjectFiles,
    /// A file named by an `--output` flag.
    OutputFile,
    /// Database schema or data.
    Database,
    /// Cargo's build output.
    BuildArtifacts,
    /// Runs the application or its tests, which may do anything.
    RunsCode,
    /// Installs Rust toolchains or components (only when asked).
    Toolchain,
    /// Containers and volumes of the local container runtime.
    Containers,
}

/// Whether a command connects to live services.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Live {
    /// Never.
    Never,
    /// Only when a flag asks for it (`--live`).
    OptIn,
    /// Always.
    Always,
}

/// An output format a command offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// Human-readable text.
    Text,
    /// A versioned JSON envelope (`--json`).
    Json,
    /// Markdown.
    Markdown,
    /// A raw document (the OpenAPI JSON).
    Document,
    /// Model Context Protocol messages on stdin and stdout.
    Mcp,
}

/// Help section of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    /// Create, run and inspect an app.
    App,
    /// Schema migrations.
    Migrations,
    /// Context for AI tools.
    Ai,
    /// Cargo passthroughs.
    Cargo,
    /// The tool itself.
    Tool,
}

impl Group {
    const fn title(self) -> &'static str {
        match self {
            Self::App => "Create, run and inspect an app",
            Self::Migrations => "Migrations",
            Self::Ai => "AI tools",
            Self::Cargo => "Cargo (arguments are forwarded unchanged)",
            Self::Tool => "This tool",
        }
    }
}

/// One command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Command {
    /// The name, with its subcommand (`openapi export`).
    pub name: &'static str,
    /// Help section.
    pub group: Group,
    /// Usage after `siderite`.
    pub usage: &'static str,
    /// One-line description.
    pub summary: &'static str,
    /// Where it runs.
    pub runner: Runner,
    /// What it needs.
    pub needs: Needs,
    /// What it may change; empty for read-only commands.
    pub mutates: &'static [Mutation],
    /// Live service access.
    pub live: Live,
    /// Output formats.
    pub formats: &'static [Format],
}

impl Command {
    /// The first word of the name (what dispatch matches).
    pub fn word(&self) -> &'static str {
        self.name.split(' ').next().unwrap_or(self.name)
    }

    /// Whether `--json` applies.
    pub fn json(&self) -> bool {
        self.formats.contains(&Format::Json)
    }
}

type Text = &'static str;

const TEXT: &[Format] = &[Format::Text];
const TEXT_JSON: &[Format] = &[Format::Text, Format::Json];
const NONE: &[Mutation] = &[];
const CONTAINERS: Mutation = Mutation::Containers;
const CONTAINER_FILES: &[Mutation] = &[CONTAINERS, Mutation::ProjectFiles];

const fn cmd(
    name: &'static str,
    group: Group,
    usage: &'static str,
    summary: &'static str,
) -> Command {
    Command {
        name,
        group,
        usage,
        summary,
        runner: Runner::App,
        needs: Needs::App,
        mutates: NONE,
        live: Live::Never,
        formats: TEXT,
    }
}

const fn cargo(
    name: &'static str,
    usage: &'static str,
    summary: &'static str,
    sub: (&'static str, bool, Option<&'static str>),
) -> Command {
    let (cargo, takes_bin, component) = sub;
    Command {
        runner: Runner::Cargo {
            cargo,
            takes_bin,
            component,
        },
        needs: Needs::Package,
        mutates: &[Mutation::BuildArtifacts],
        ..cmd(name, Group::Cargo, usage, summary)
    }
}

const fn migration(name: Text, usage: Text, summary: Text) -> Command {
    Command {
        runner: Runner::AppOrStandalone,
        needs: Needs::Nothing,
        mutates: &[Mutation::Database],
        live: Live::Always,
        ..cmd(name, Group::Migrations, usage, summary)
    }
}

/// Every command, in help order.
pub const COMMANDS: &[Command] = &[
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        mutates: &[Mutation::ProjectFiles],
        ..cmd(
            "new",
            Group::App,
            "new NAME [--database E] [--template T] [--path DIR] [--dry-run]",
            "Write a new API crate",
        )
    },
    Command {
        runner: Runner::Tool,
        mutates: &[Mutation::BuildArtifacts, Mutation::RunsCode],
        ..cmd(
            "dev",
            Group::App,
            "dev [--grace-ms N] [-- ARGS]",
            "Rebuild and restart the app on changes",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Package,
        mutates: &[Mutation::ProjectFiles],
        ..cmd(
            "generate resource",
            Group::App,
            "generate resource NAME [FIELD:TYPE]... [--table T]",
            "Write a model and its CRUD routes",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Package,
        mutates: &[Mutation::ProjectFiles],
        ..cmd(
            "generate test",
            Group::App,
            "generate test route|resource NAME ... [--dry-run]",
            "Write a request/response test (tests/NAME.rs)",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Package,
        mutates: &[Mutation::ProjectFiles],
        ..cmd(
            "generate ci",
            Group::App,
            "generate ci [--update] [--dry-run]",
            "Write a GitHub Actions verify workflow",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Package,
        mutates: &[Mutation::ProjectFiles],
        ..cmd(
            "generate model",
            Group::App,
            "generate model NAME [FIELD:TYPE]... [--table T] [--dry-run]",
            "Write a model (src/models/NAME.rs)",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Package,
        mutates: &[Mutation::ProjectFiles],
        ..cmd(
            "generate route",
            Group::App,
            "generate route NAME [--path /URL] [--dry-run]",
            "Write a route module (src/routes/NAME.rs)",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Package,
        mutates: &[Mutation::ProjectFiles],
        ..cmd(
            "add",
            Group::App,
            "add CRATE... | add --preset postgres|mysql [--dry-run]",
            "cargo add for the app package, or a preset",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Package,
        mutates: &[Mutation::ProjectFiles],
        ..cmd(
            "remove",
            Group::App,
            "remove CRATE... [--dry-run]",
            "cargo remove for the app package",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        mutates: &[Mutation::Toolchain],
        formats: TEXT_JSON,
        ..cmd(
            "setup",
            Group::App,
            "setup [--json] [--install]",
            "Check build prerequisites; say how to fix",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        live: Live::OptIn,
        formats: TEXT_JSON,
        ..cmd(
            "doctor",
            Group::App,
            "doctor [--json] [--live [--database ALIAS]...]",
            "Check the toolchain, project and configuration",
        )
    },
    Command {
        runner: Runner::Tool,
        mutates: &[Mutation::BuildArtifacts, Mutation::RunsCode],
        live: Live::OptIn,
        formats: TEXT_JSON,
        ..cmd(
            "verify",
            Group::App,
            "verify [--json] [--live]",
            "fmt, lint, build, test and check in one report",
        )
    },
    Command {
        mutates: &[Mutation::RunsCode],
        live: Live::Always,
        ..cmd("run", Group::App, "run [--addr ADDR]", "Serve the app")
    },
    Command {
        formats: TEXT_JSON,
        ..cmd(
            "routes",
            Group::App,
            "routes [--json]",
            "List the documented routes",
        )
    },
    Command {
        formats: TEXT_JSON,
        ..cmd(
            "check",
            Group::App,
            "check [--json]",
            "Validate config, models, migrations and routes",
        )
    },
    Command {
        mutates: &[Mutation::OutputFile],
        formats: &[Format::Document],
        ..cmd(
            "openapi export",
            Group::App,
            "openapi export [--output PATH] [--force]",
            "Write the OpenAPI document (stdout or PATH)",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        formats: &[Format::Mcp],
        ..cmd(
            "mcp",
            Group::Ai,
            "mcp",
            "Read-only MCP tools on stdio (`mcp` feature)",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Package,
        formats: TEXT_JSON,
        ..cmd(
            "upgrade",
            Group::Tool,
            "upgrade --to VERSION --dry-run [--json]",
            "Preview a framework upgrade (writes nothing)",
        )
    },
    Command {
        runner: Runner::Tool,
        mutates: &[Mutation::BuildArtifacts, Mutation::RunsCode],
        formats: TEXT_JSON,
        ..cmd(
            "package",
            Group::App,
            "package --release [--output DIR] [--force] [--json]",
            "Build a release directory with checksums",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        formats: TEXT_JSON,
        ..cmd(
            "openapi diff",
            Group::App,
            "openapi diff BASE CURRENT [--json]",
            "Compare two OpenAPI documents for clients",
        )
    },
    Command {
        live: Live::Always,
        ..cmd(
            "dbshell",
            Group::App,
            "dbshell",
            "Open the database's native client",
        )
    },
    // Needs the compiled models, so only the app binary can run it.
    Command {
        runner: Runner::App,
        needs: Needs::App,
        mutates: &[Mutation::ProjectFiles],
        live: Live::Never,
        ..migration(
            "makemigrations",
            "makemigrations [--name SLUG] [--empty] [--dry-run]",
            "Write a migration for model changes",
        )
    },
    migration(
        "migrate",
        "migrate [TARGET] [--dry-run]",
        "Apply migrations",
    ),
    migration(
        "rollback",
        "rollback [--steps N | TARGET] [--dry-run]",
        "Unapply migrations",
    ),
    Command {
        mutates: NONE,
        formats: TEXT_JSON,
        ..migration(
            "showmigrations",
            "showmigrations [--json]",
            "List migrations and whether they are applied",
        )
    },
    Command {
        mutates: NONE,
        ..migration(
            "inspectmigrations",
            "inspectmigrations",
            "Read-only report of interrupted migrations",
        )
    },
    Command {
        mutates: &[Mutation::ProjectFiles],
        live: Live::Never,
        ..migration(
            "squashmigrations",
            "squashmigrations FROM TO [--name SLUG]",
            "Collapse a range of migrations into one",
        )
    },
    Command {
        live: Live::OptIn,
        formats: &[Format::Markdown, Format::Json],
        ..cmd(
            "ai dump-context",
            Group::Ai,
            "ai dump-context [--format markdown|json] [--live]",
            "Application context for AI tools",
        )
    },
    Command {
        formats: &[Format::Markdown, Format::Json],
        ..cmd(
            "ai context",
            Group::Ai,
            "ai context --task TEXT [--budget N] [--include PATH]... \
             [--json]",
            "Context for one task, within a byte budget",
        )
    },
    Command {
        runner: Runner::Tool,
        mutates: &[Mutation::BuildArtifacts, Mutation::RunsCode],
        live: Live::OptIn,
        formats: TEXT_JSON,
        ..cmd(
            "ai validate",
            Group::Ai,
            "ai validate [--json] [--live]",
            "verify, as findings with fixes and docs",
        )
    },
    Command {
        mutates: &[Mutation::ProjectFiles],
        ..cmd(
            "ai init",
            Group::Ai,
            "ai init [--check] [--dry-run] [--force] [--link FILE]",
            "Write or refresh .siderite/context.md",
        )
    },
    cargo(
        "build",
        "build [ARGS]",
        "cargo build in the app package",
        ("build", true, None),
    ),
    Command {
        mutates: &[Mutation::BuildArtifacts, Mutation::RunsCode],
        ..cargo(
            "test",
            "test [ARGS]",
            "cargo test in the app package",
            ("test", true, None),
        )
    },
    Command {
        mutates: &[Mutation::ProjectFiles],
        ..cargo(
            "fmt",
            "fmt [--check ...]",
            "cargo fmt (rustfmt) in the app package",
            ("fmt", false, Some("rustfmt")),
        )
    },
    cargo(
        "lint",
        "lint [-- -D warnings ...]",
        "cargo clippy (Rust lints, not `check`)",
        ("clippy", true, Some("clippy")),
    ),
    cargo(
        "clean",
        "clean [ARGS]",
        "cargo clean (only when you run it)",
        ("clean", false, None),
    ),
    Command {
        mutates: &[Mutation::Database, Mutation::RunsCode],
        live: Live::Always,
        formats: TEXT_JSON,
        ..cmd(
            "seed",
            Group::Migrations,
            "seed --dataset NAME | seed --list [--json]",
            "Load a seed dataset the app registers",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        mutates: CONTAINER_FILES,
        live: Live::Always,
        formats: TEXT_JSON,
        ..cmd(
            "services up",
            Group::App,
            "services up [postgres] [--port N] [--image I] [--json]",
            "Start local PostgreSQL in a container",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        mutates: &[CONTAINERS],
        live: Live::Always,
        formats: TEXT_JSON,
        ..cmd(
            "services down",
            Group::App,
            "services down [postgres] [--json]",
            "Stop local services, keeping their data",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        live: Live::Always,
        formats: TEXT_JSON,
        ..cmd(
            "services status",
            Group::App,
            "services status [--json]",
            "Show local services",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        ..cmd(
            "services env",
            Group::App,
            "services env",
            "Print DATABASE_URL of local services",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        mutates: CONTAINER_FILES,
        live: Live::Always,
        formats: TEXT_JSON,
        ..cmd(
            "services destroy",
            Group::App,
            "services destroy postgres --yes [--json]",
            "Delete a local service and its data",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        formats: TEXT_JSON,
        ..cmd(
            "config show",
            Group::Tool,
            "config show [--redacted] [--json]",
            "Show config values and their sources",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        formats: TEXT_JSON,
        ..cmd(
            "config validate",
            Group::Tool,
            "config validate [--json]",
            "Load config, check database URLs",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        formats: TEXT_JSON,
        ..cmd(
            "docs search",
            Group::Tool,
            "docs search QUERY... [--json] [--limit N]",
            "Search this version's framework docs offline",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        formats: TEXT_JSON,
        ..cmd(
            "explain",
            Group::Tool,
            "explain CODE [--json] | explain --list [--json]",
            "Explain a diagnostic code and how to fix it",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        formats: TEXT_JSON,
        ..cmd(
            "commands",
            Group::Tool,
            "commands [--json]",
            "List commands, their needs and changes",
        )
    },
    Command {
        runner: Runner::Tool,
        needs: Needs::Nothing,
        ..cmd(
            "completions",
            Group::Tool,
            "completions bash|zsh|fish",
            "Print a shell completion script",
        )
    },
];

/// The command for `word` (the first word, as typed).
pub fn find(word: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.word() == word)
}

/// The command named by the positional words of `args` (`ai init` before
/// `ai`), ignoring flags and the values of global flags.
pub fn resolve(args: &[String]) -> Option<&'static Command> {
    let words = positional(args);
    let first = words.first()?;
    let two = words.get(1).map(|second| format!("{first} {second}"));
    let exact = two.and_then(|two| COMMANDS.iter().find(|c| c.name == two));
    exact.or_else(|| find(first))
}

/// Positional words, skipping flags and the values of value flags.
fn positional(args: &[String]) -> Vec<&str> {
    const VALUED: [&str; 6] = [
        "--database-url",
        "--database",
        "--addr",
        "--migrations-dir",
        "--manifest-path",
        "--bin",
    ];
    let mut words = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let package = arg == "-p" || arg == "--package";
        if VALUED.contains(&arg.as_str()) || package {
            args.next();
        } else if !arg.starts_with('-') {
            words.push(arg.as_str());
        }
    }
    words
}

/// The data of the `commands` envelope.
#[derive(Debug, Clone, Serialize)]
pub struct CommandList {
    /// [`COMMANDS_VERSION`].
    pub version: u32,
    /// The framework version.
    pub siderite: &'static str,
    /// The commands.
    pub commands: &'static [Command],
}

/// The command list for `siderite commands --json`.
pub fn list() -> CommandList {
    CommandList {
        version: COMMANDS_VERSION,
        siderite: env!("CARGO_PKG_VERSION"),
        commands: COMMANDS,
    }
}

/// Help lines for the commands `filter` keeps, grouped and aligned.
pub fn help(filter: impl Fn(&Command) -> bool) -> String {
    let mut out = String::new();
    let mut group = None;
    for command in COMMANDS.iter().filter(|c| filter(c)) {
        if group != Some(command.group) {
            group = Some(command.group);
            out.push_str(&format!("\n{}:\n", command.group.title()));
        }
        let (usage, summary) = (command.usage, command.summary);
        if usage.len() <= 30 {
            out.push_str(&format!("  {usage:<30}  {summary}\n"));
        } else {
            out.push_str(&format!("  {usage}\n  {:<30}  {summary}\n", ""));
        }
    }
    out
}

/// The text form of `siderite commands`: what each command needs, may
/// change and touches.
pub fn table() -> String {
    let mut out = format!(
        "{:<18} {:<8} {:<26} {:<8} {}\n",
        "COMMAND", "NEEDS", "MAY CHANGE", "LIVE", "FORMATS"
    );
    for c in COMMANDS {
        let needs = serde_name(&c.needs);
        let mutates: Vec<String> = c.mutates.iter().map(serde_name).collect();
        let mutates = if mutates.is_empty() {
            "-".to_owned()
        } else {
            mutates.join(",")
        };
        let formats: Vec<String> = c.formats.iter().map(serde_name).collect();
        out.push_str(&format!(
            "{:<18} {needs:<8} {mutates:<26} {:<8} {}\n",
            c.name,
            serde_name(&c.live),
            formats.join(",")
        ));
    }
    out
}

/// The serialized (`snake_case`) name of a unit variant.
fn serde_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_and_help_fits_eighty_columns() {
        let args = |list: &[&str]| -> Vec<String> {
            let owned = list.iter().map(|a| (*a).to_owned());
            owned.collect()
        };
        let init = resolve(&args(&["--json", "ai", "init"])).map(|c| c.name);
        assert_eq!(init, Some("ai init"));
        let dump = resolve(&args(&["ai", "--live", "dump-context"]));
        assert_eq!(dump.map(|c| c.name), Some("ai dump-context"));
        let check = resolve(&args(&["--database", "x", "check"]));
        assert_eq!(check.map(|c| c.name), Some("check"));
        assert!(resolve(&args(&["frobnicate"])).is_none());
        for line in help(|_| true).lines() {
            assert!(line.chars().count() <= 80, "{line}");
        }
        let mut names: Vec<_> = COMMANDS.iter().map(|c| c.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), COMMANDS.len(), "duplicate command names");
    }

    #[test]
    fn declarations_are_consistent() {
        for c in COMMANDS {
            let cargo = matches!(c.runner, Runner::Cargo { .. });
            assert_eq!(cargo, c.group == Group::Cargo, "{}", c.name);
            if c.live == Live::OptIn {
                assert!(c.usage.contains("--live"), "{}", c.name);
            }
            if c.json() {
                assert!(c.usage.contains("json"), "{}", c.name);
            }
        }
    }
}
