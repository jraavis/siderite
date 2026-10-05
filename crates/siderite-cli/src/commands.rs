//! Command metadata and contract for developer and AI tooling.

use serde::{Deserialize, Serialize};

/// Project prerequisites needed to execute a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRequirement {
    /// Can execute outside any project directory.
    None,
    /// Requires an application package directory.
    PackageRequired,
    /// Runs in a package or standalone with explicit database URL.
    PackageOrStandalone,
}

/// State mutations performed by a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationKind {
    /// Read-only command; leaves environment unchanged.
    None,
    /// Creates or modifies files on the filesystem.
    Filesystem,
    /// Modifies database schema or data.
    Database,
    /// Creates or modifies both files and database.
    FilesystemAndDatabase,
    /// Compiles code or spawns a long-running/test process.
    Process,
}

/// Network or external service requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveAccess {
    /// Fully offline; no external service or network access needed.
    None,
    /// Requires an active database connection.
    Database,
    /// Binds to a network address or communicates across network.
    Network,
    /// Requires both database connection and network socket binding.
    DatabaseAndNetwork,
}

/// Output formats supported by a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// Human-readable text on standard output.
    Text,
    /// Machine-readable versioned JSON on standard output.
    Json,
    /// Interactive terminal sessions (e.g. database shell).
    Interactive,
    /// Direct output forwarding from an underlying child process.
    Passthrough,
}

/// Metadata describing a command-line flag or option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagMeta {
    /// Long flag name, including leading hyphens (e.g. `--addr`).
    pub name: String,
    /// Short single-letter alias, if any.
    pub short: Option<char>,
    /// Metavar argument name (e.g. `ADDR`), if the flag takes a value.
    pub arg_name: Option<String>,
    /// Human-readable description of what the flag controls.
    pub description: String,
    /// Whether the flag is required for invocation.
    pub required: bool,
}

impl FlagMeta {
    /// Create an option flag that takes a value.
    pub fn opt(name: &str, short: Option<char>, arg_name: &str, desc: &str) -> Self {
        Self {
            name: name.to_owned(),
            short,
            arg_name: Some(arg_name.to_owned()),
            description: desc.to_owned(),
            required: false,
        }
    }

    /// Create a boolean switch flag that takes no value.
    pub fn flag(name: &str, short: Option<char>, desc: &str) -> Self {
        Self {
            name: name.to_owned(),
            short,
            arg_name: None,
            description: desc.to_owned(),
            required: false,
        }
    }
}

/// Machine-readable metadata describing a developer command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandMeta {
    /// Command name invoked on the CLI.
    pub name: String,
    /// One-line summary for help tables.
    pub summary: String,
    /// Detailed description for AI and documentation tools.
    pub description: String,
    /// Usage syntax example.
    pub usage: String,
    /// Logical category (e.g. "app", "migration", "cargo").
    pub category: String,
    /// Project context requirement.
    pub project_requirement: ProjectRequirement,
    /// Mutation characteristics.
    pub mutation: MutationKind,
    /// External service requirements.
    pub live_access: LiveAccess,
    /// Supported output modes.
    pub output_modes: Vec<OutputMode>,
    /// Command-specific flags and options.
    pub flags: Vec<FlagMeta>,
}

/// Return metadata for all standard developer commands.
#[must_use]
pub fn command_catalog() -> Vec<CommandMeta> {
    vec![
        CommandMeta {
            name: "new".into(),
            summary: "Write a new API crate".into(),
            description: "Scaffolds a new Siderite API project with routing and config.".into(),
            usage: "siderite new <NAME>".into(),
            category: "app".into(),
            project_requirement: ProjectRequirement::None,
            mutation: MutationKind::Filesystem,
            live_access: LiveAccess::None,
            output_modes: vec![OutputMode::Text],
            flags: vec![],
        },
        CommandMeta {
            name: "run".into(),
            summary: "Serve the app".into(),
            description: "Connects databases and runs the HTTP server listener.".into(),
            usage: "siderite run [--addr ADDR]".into(),
            category: "app".into(),
            project_requirement: ProjectRequirement::PackageRequired,
            mutation: MutationKind::Process,
            live_access: LiveAccess::DatabaseAndNetwork,
            output_modes: vec![OutputMode::Text],
            flags: vec![FlagMeta::opt(
                "--addr",
                None,
                "ADDR",
                "Listen address (default 127.0.0.1:8000)",
            )],
        },
        CommandMeta {
            name: "routes".into(),
            summary: "List METHOD PATH operation_id".into(),
            description: "Prints documented HTTP routes in the application.".into(),
            usage: "siderite routes [--json]".into(),
            category: "app".into(),
            project_requirement: ProjectRequirement::PackageRequired,
            mutation: MutationKind::None,
            live_access: LiveAccess::None,
            output_modes: vec![OutputMode::Text, OutputMode::Json],
            flags: vec![FlagMeta::flag(
                "--json",
                None,
                "Output as structured JSON envelope",
            )],
        },
        CommandMeta {
            name: "check".into(),
            summary: "Validate config, models, migrations and routes".into(),
            description: "Runs framework diagnostic checks without starting server.".into(),
            usage: "siderite check [--json]".into(),
            category: "app".into(),
            project_requirement: ProjectRequirement::PackageRequired,
            mutation: MutationKind::None,
            live_access: LiveAccess::None,
            output_modes: vec![OutputMode::Text, OutputMode::Json],
            flags: vec![FlagMeta::flag(
                "--json",
                None,
                "Output as structured JSON envelope",
            )],
        },
        CommandMeta {
            name: "dbshell".into(),
            summary: "Open the database's native client".into(),
            description: "Launches sqlite3, psql, or mysql with connection env.".into(),
            usage: "siderite dbshell [--database ALIAS]".into(),
            category: "app".into(),
            project_requirement: ProjectRequirement::PackageOrStandalone,
            mutation: MutationKind::Database,
            live_access: LiveAccess::Database,
            output_modes: vec![OutputMode::Interactive],
            flags: vec![FlagMeta::opt(
                "--database",
                None,
                "ALIAS",
                "Database alias from settings (default: default)",
            )],
        },
        CommandMeta {
            name: "build".into(),
            summary: "cargo build in the app package".into(),
            description: "Compiles the application binary forwarding arguments \
                 to cargo."
                .into(),
            usage: "siderite build [--release ...]".into(),
            category: "cargo".into(),
            project_requirement: ProjectRequirement::PackageRequired,
            mutation: MutationKind::Filesystem,
            live_access: LiveAccess::None,
            output_modes: vec![OutputMode::Passthrough],
            flags: vec![],
        },
        CommandMeta {
            name: "test".into(),
            summary: "cargo test in the app package".into(),
            description: "Runs package unit and integration tests via cargo test.".into(),
            usage: "siderite test [...]".into(),
            category: "cargo".into(),
            project_requirement: ProjectRequirement::PackageRequired,
            mutation: MutationKind::Process,
            live_access: LiveAccess::None,
            output_modes: vec![OutputMode::Passthrough],
            flags: vec![],
        },
        CommandMeta {
            name: "makemigrations".into(),
            summary: "Write a new migration from models".into(),
            description: "Computes schema diff from compiled models and creates JSON.".into(),
            usage: "siderite makemigrations [--name SLUG] [--empty] [--dry-run]".into(),
            category: "migration".into(),
            project_requirement: ProjectRequirement::PackageRequired,
            mutation: MutationKind::Filesystem,
            live_access: LiveAccess::None,
            output_modes: vec![OutputMode::Text],
            flags: vec![
                FlagMeta::opt("--name", None, "SLUG", "Slug for migration file"),
                FlagMeta::flag("--empty", None, "Write empty migration"),
                FlagMeta::flag("--dry-run", None, "Print planned operations"),
            ],
        },
        CommandMeta {
            name: "migrate".into(),
            summary: "Apply migrations".into(),
            description: "Applies pending migrations up to target or latest.".into(),
            usage: "siderite migrate [TARGET] [--dry-run]".into(),
            category: "migration".into(),
            project_requirement: ProjectRequirement::PackageOrStandalone,
            mutation: MutationKind::Database,
            live_access: LiveAccess::Database,
            output_modes: vec![OutputMode::Text],
            flags: vec![FlagMeta::flag(
                "--dry-run",
                None,
                "Print SQL without executing",
            )],
        },
        CommandMeta {
            name: "rollback".into(),
            summary: "Unapply migrations".into(),
            description: "Rolls back applied migrations by steps or target.".into(),
            usage: "siderite rollback [--steps N | TARGET] [--dry-run]".into(),
            category: "migration".into(),
            project_requirement: ProjectRequirement::PackageOrStandalone,
            mutation: MutationKind::Database,
            live_access: LiveAccess::Database,
            output_modes: vec![OutputMode::Text],
            flags: vec![
                FlagMeta::opt("--steps", None, "N", "Number of steps to roll back"),
                FlagMeta::flag("--dry-run", None, "Print SQL without executing"),
            ],
        },
        CommandMeta {
            name: "showmigrations".into(),
            summary: "List migrations and applied status".into(),
            description: "Displays available migrations and indicates applied state.".into(),
            usage: "siderite showmigrations".into(),
            category: "migration".into(),
            project_requirement: ProjectRequirement::PackageOrStandalone,
            mutation: MutationKind::None,
            live_access: LiveAccess::Database,
            output_modes: vec![OutputMode::Text],
            flags: vec![],
        },
        CommandMeta {
            name: "inspectmigrations".into(),
            summary: "Read-only recovery report".into(),
            description: "Inspects migration state, intents and crash boundaries.".into(),
            usage: "siderite inspectmigrations".into(),
            category: "migration".into(),
            project_requirement: ProjectRequirement::PackageOrStandalone,
            mutation: MutationKind::None,
            live_access: LiveAccess::Database,
            output_modes: vec![OutputMode::Text],
            flags: vec![],
        },
        CommandMeta {
            name: "squashmigrations".into(),
            summary: "Collapse a range of migrations into one".into(),
            description: "Squashes historical migrations into a consolidated migration.".into(),
            usage: "siderite squashmigrations FROM TO [--name SLUG]".into(),
            category: "migration".into(),
            project_requirement: ProjectRequirement::PackageOrStandalone,
            mutation: MutationKind::Filesystem,
            live_access: LiveAccess::None,
            output_modes: vec![OutputMode::Text],
            flags: vec![FlagMeta::opt(
                "--name",
                None,
                "SLUG",
                "Name for squashed migration",
            )],
        },
        CommandMeta {
            name: "commands".into(),
            summary: "List available commands and metadata".into(),
            description: "Returns machine-readable developer command catalog.".into(),
            usage: "siderite commands [--json]".into(),
            category: "meta".into(),
            project_requirement: ProjectRequirement::None,
            mutation: MutationKind::None,
            live_access: LiveAccess::None,
            output_modes: vec![OutputMode::Text, OutputMode::Json],
            flags: vec![FlagMeta::flag(
                "--json",
                None,
                "Output as structured JSON envelope",
            )],
        },
    ]
}

/// Find a specific command by name in the catalog.
#[must_use]
pub fn find_command(name: &str) -> Option<CommandMeta> {
    command_catalog().into_iter().find(|c| c.name == name)
}

/// Format the command catalog as human-readable text.
#[must_use]
pub fn render_commands_text(commands: &[CommandMeta]) -> String {
    let mut out = String::new();
    out.push_str("COMMAND           REQUIREMENT      MUTATION   LIVE   SUMMARY\n");
    for cmd in commands {
        let req = match cmd.project_requirement {
            ProjectRequirement::None => "none",
            ProjectRequirement::PackageRequired => "package",
            ProjectRequirement::PackageOrStandalone => "pkg/url",
        };
        let mut_kind = match cmd.mutation {
            MutationKind::None => "read-only",
            MutationKind::Filesystem => "fs",
            MutationKind::Database => "db",
            MutationKind::FilesystemAndDatabase => "fs+db",
            MutationKind::Process => "process",
        };
        let live = match cmd.live_access {
            LiveAccess::None => "offline",
            LiveAccess::Database => "db",
            LiveAccess::Network => "net",
            LiveAccess::DatabaseAndNetwork => "db+net",
        };
        out.push_str(&format!(
            "{:<17} {:<16} {:<10} {:<6} {}\n",
            cmd.name, req, mut_kind, live, cmd.summary
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_all_core_commands() {
        let catalog = command_catalog();
        let names: Vec<&str> = catalog.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"new"));
        assert!(names.contains(&"run"));
        assert!(names.contains(&"routes"));
        assert!(names.contains(&"check"));
        assert!(names.contains(&"build"));
        assert!(names.contains(&"test"));
        assert!(names.contains(&"commands"));
        assert!(names.contains(&"migrate"));
        assert!(names.contains(&"makemigrations"));
    }

    #[test]
    fn commands_declare_contracts_accurately() {
        let routes = find_command("routes").unwrap();
        assert_eq!(
            routes.project_requirement,
            ProjectRequirement::PackageRequired
        );
        assert_eq!(routes.mutation, MutationKind::None);
        assert_eq!(routes.live_access, LiveAccess::None);
        assert!(routes.output_modes.contains(&OutputMode::Json));

        let migrate = find_command("migrate").unwrap();
        assert_eq!(
            migrate.project_requirement,
            ProjectRequirement::PackageOrStandalone
        );
        assert_eq!(migrate.mutation, MutationKind::Database);
        assert_eq!(migrate.live_access, LiveAccess::Database);

        let new_cmd = find_command("new").unwrap();
        assert_eq!(new_cmd.project_requirement, ProjectRequirement::None);
        assert_eq!(new_cmd.mutation, MutationKind::Filesystem);
        assert_eq!(new_cmd.live_access, LiveAccess::None);
    }

    #[test]
    fn render_commands_text_produces_table() {
        let catalog = command_catalog();
        let rendered = render_commands_text(&catalog);
        assert!(rendered.contains("COMMAND"));
        assert!(rendered.contains("routes"));
        assert!(rendered.contains("read-only"));
    }
}
