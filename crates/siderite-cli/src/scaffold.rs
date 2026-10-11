//! `siderite new`: write a small API crate.

use crate::error::CliError;
use crate::generate::{Plan, ident, preview};
use std::path::{Path, PathBuf};

const GIT_DEP: &str = r#"git = "https://github.com/jraavis/siderite""#;

/// Database engine for a new project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    Sqlite,
    Postgres,
    Mysql,
}

impl Engine {
    fn parse(value: &str) -> Result<Self, CliError> {
        match value {
            "sqlite" => Ok(Self::Sqlite),
            "postgres" => Ok(Self::Postgres),
            "mysql" => Ok(Self::Mysql),
            other => Err(CliError::usage(format!(
                "unknown --database `{other}` (sqlite, postgres or mysql)"
            ))),
        }
    }

    fn url(self, crate_name: &str) -> String {
        match self {
            Self::Sqlite => format!("sqlite://{crate_name}.db?mode=rwc"),
            Self::Postgres => {
                format!("postgres://siderite:siderite@127.0.0.1:5432/{crate_name}")
            }
            Self::Mysql => format!("mysql://siderite:siderite@127.0.0.1:3306/{crate_name}"),
        }
    }

    fn cli_features(self) -> &'static str {
        match self {
            Self::Sqlite => "",
            Self::Postgres => r#", features = ["postgres"]"#,
            Self::Mysql => r#", features = ["mysql"]"#,
        }
    }

    fn backends_dep(self, loc: &str) -> String {
        match self {
            Self::Sqlite => String::new(),
            Self::Postgres => {
                format!("siderite-backends = {{ {loc}, features = [\"postgres\"] }}\n")
            }
            Self::Mysql => {
                format!("siderite-backends = {{ {loc}, features = [\"mysql\"] }}\n")
            }
        }
    }
}

/// `siderite new NAME [--database ENGINE] [--path DIR] [--dry-run]`.
pub fn run(args: &[String]) -> Result<u8, CliError> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(0);
    }
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let kept = args.iter().filter(|a| *a != "--dry-run").cloned();
    let args: Vec<String> = kept.collect();
    let (name, engine, path) = parse(&args)?;
    let dest = std::env::current_dir()
        .map_err(|err| io_err("read cwd", err))?
        .join(&name);
    let plan = project_plan(&dest, &name, engine, path.as_deref())?;
    if dry_run {
        print!("{}", preview(&plan.check()?));
        return Ok(0);
    }
    plan.apply()?;
    println!("Created {name}. Next:");
    println!("  cd {name}");
    println!("  siderite run");
    Ok(0)
}

fn parse(args: &[String]) -> Result<(String, Engine, Option<PathBuf>), CliError> {
    let mut name = None;
    let mut engine = Engine::Sqlite;
    let mut path = None;
    let mut i = 0;
    // `args` starts at `new`.
    if args.first().map(String::as_str) == Some("new") {
        i = 1;
    }
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--database" || arg.starts_with("--database=") {
            let value = if let Some(v) = arg.strip_prefix("--database=") {
                v.to_owned()
            } else {
                let v = args
                    .get(i)
                    .ok_or_else(|| CliError::usage("--database requires a value"))?;
                i += 1;
                v.clone()
            };
            engine = Engine::parse(&value)?;
            continue;
        }
        if arg == "--path" || arg.starts_with("--path=") {
            let value = if let Some(v) = arg.strip_prefix("--path=") {
                v.to_owned()
            } else {
                let v = args
                    .get(i)
                    .ok_or_else(|| CliError::usage("--path requires a value"))?;
                i += 1;
                v.clone()
            };
            path = Some(PathBuf::from(value));
            continue;
        }
        if arg.starts_with('-') {
            return Err(CliError::usage(format!("unknown flag `{arg}` for `new`")));
        }
        if name.is_some() {
            return Err(CliError::usage("unexpected extra argument for `new`"));
        }
        name = Some(arg.clone());
    }
    let name = name.ok_or_else(|| CliError::usage("usage: siderite new NAME"))?;
    ident::package_name(&name)?;
    Ok((name, engine, path))
}

/// The files of a new project in `dest`, which must not exist or be
/// empty.
fn project_plan(
    dest: &Path,
    name: &str,
    engine: Engine,
    siderite_root: Option<&Path>,
) -> Result<Plan, CliError> {
    if dest.exists() {
        let empty = dest
            .read_dir()
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if !empty {
            return Err(CliError::Io(format!(
                "`{}` already exists and is not empty",
                dest.display()
            )));
        }
    }
    let deps = Deps::new(engine, siderite_root)?;
    let ident = name.replace('-', "_");
    let url = engine.url(name);
    let mut plan = Plan::new(dest);
    let files = [
        ("Cargo.toml", cargo_toml(name, &deps)),
        ("siderite.toml", siderite_toml(name, &url)),
        ("src/lib.rs", lib_rs(&ident)),
        ("src/main.rs", main_rs(&ident)),
        ("migrations/.gitkeep", String::new()),
        (".gitignore", "/target\n*.db\n.env\n".to_owned()),
        ("README.md", readme(name)),
    ];
    for (path, contents) in files {
        plan.add(path, contents)?;
    }
    Ok(plan)
}

/// Where the new project gets its siderite crates from.
struct Deps {
    siderite: String,
    cli: String,
    backends: String,
}

impl Deps {
    fn new(engine: Engine, root: Option<&Path>) -> Result<Self, CliError> {
        let Some(root) = root else {
            return Ok(Self {
                siderite: GIT_DEP.to_owned(),
                cli: format!("{GIT_DEP}{}", engine.cli_features()),
                backends: engine.backends_dep(GIT_DEP),
            });
        };
        let root = root
            .canonicalize()
            .map_err(|err| io_err("resolve --path", err))?;
        let crate_path = |pkg: &str| {
            let path = root.join("crates").join(pkg);
            format!("path = \"{}\"", path.display()).replace('\\', "/")
        };
        Ok(Self {
            siderite: crate_path("siderite"),
            cli: crate_path("siderite-cli") + engine.cli_features(),
            backends: engine.backends_dep(&crate_path("siderite-backends")),
        })
    }
}

fn cargo_toml(name: &str, deps: &Deps) -> String {
    let Deps {
        siderite,
        cli,
        backends,
    } = deps;
    format!(
        "\
[package]
name = \"{name}\"
version = \"0.1.0\"
edition = \"2024\"

[dependencies]
siderite = {{ {siderite} }}
siderite-cli = {{ {cli} }}
{backends}tokio = {{ version = \"1\", features = [\"macros\", \"rt-multi-thread\"] }}
serde = {{ version = \"1\", features = [\"derive\"] }}
"
    )
}

fn siderite_toml(name: &str, url: &str) -> String {
    format!(
        "\
[app]
name = \"{name}\"

[server]
addr = \"127.0.0.1:8000\"

[databases.default]
url = \"{url}\"
"
    )
}

fn readme(name: &str) -> String {
    format!(
        "\
# {name}

```bash
siderite run
```

Open http://127.0.0.1:8000/ and http://127.0.0.1:8000/docs.
"
    )
}

fn lib_rs(ident: &str) -> String {
    format!(
        "\
//! {ident} HTTP API.

use siderite::prelude::*;

/// Greeting.
#[get(\"/\")]
async fn index() -> PlainText<&'static str> {{
    PlainText(\"Hello, siderite!\")
}}

/// The application.
pub fn app() -> App {{
    App::new()
        .title(\"{ident}\")
        .version(\"0.1.0\")
        .routes(routes![index])
}}
"
    )
}

fn main_rs(ident: &str) -> String {
    format!(
        "\
//! {ident} command line.

use siderite_cli::{{AppCli, CliSettings}};

#[tokio::main]
async fn main() -> std::process::ExitCode {{
    let settings = match siderite::config::load() {{
        Ok(settings) => CliSettings::from(&settings),
        Err(err) => {{
            eprintln!(\"{{err}}\");
            return std::process::ExitCode::from(1);
        }}
    }};
    AppCli::new({ident}::app)
        .settings(settings)
        .migrations_dir(\"migrations\")
        .run()
        .await
}}
"
    )
}

fn io_err(what: &str, err: std::io::Error) -> CliError {
    CliError::Io(format!("cannot {what}: {err}"))
}

fn print_help() {
    println!(
        "\
siderite new NAME

Write a new API crate in NAME/.

Options:
  --database sqlite|postgres|mysql   Default database (sqlite)
  --path DIR                         Path to a local siderite checkout
  --dry-run                          Show the files without writing them
  --help                             Show this help
"
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn parses_name_and_flags() {
        let (name, engine, path) = parse(&args(&[
            "new",
            "demo",
            "--database",
            "postgres",
            "--path",
            "/tmp/s",
        ]))
        .unwrap();
        assert_eq!(name, "demo");
        assert_eq!(engine, Engine::Postgres);
        assert_eq!(path, Some(PathBuf::from("/tmp/s")));
    }

    #[test]
    fn rejects_bad_names() {
        assert!(parse(&args(&["new"])).is_err());
        assert!(parse(&args(&["new", "Demo"])).is_err());
        assert!(parse(&args(&["new", "1demo"])).is_err());
    }

    #[test]
    fn writes_an_api_crate() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let dir = std::env::temp_dir().join(format!(
            "siderite-new-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("demo_app");
        let sqlite = Engine::Sqlite;
        let plan = project_plan(&dest, "demo_app", sqlite, Some(&root));
        let plan = plan.unwrap();
        let text = preview(&plan.check().unwrap());
        assert!(text.contains("create src/main.rs"), "{text}");
        assert!(!dest.exists(), "the preview wrote files");
        plan.apply().unwrap();
        // Applying again changes nothing; edited files are never replaced.
        std::fs::write(dest.join("README.md"), "mine\n").unwrap();
        let again = project_plan(&dest, "demo_app", sqlite, Some(&root));
        assert!(again.is_err(), "a non-empty directory is refused");
        let cargo = std::fs::read_to_string(dir.join("demo_app/Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"demo_app\""));
        assert!(cargo.contains("siderite-cli"));
        assert!(dir.join("demo_app/src/lib.rs").is_file());
        assert!(dir.join("demo_app/src/main.rs").is_file());
        assert!(dir.join("demo_app/siderite.toml").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }
}
