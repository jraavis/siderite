//! The Cargo-free journey (X20): from prerequisites to a release package
//! using only `siderite` commands, plus the source edits a developer
//! makes by following the printed registration steps. Every step that
//! still needs Cargo directly is listed in `docs/JOURNEY.md`.
//!
//! Slow (it builds a project in debug and release), so it is ignored by
//! default; CI runs it on Linux.

#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Journey {
    project: PathBuf,
    target: PathBuf,
}

impl Journey {
    fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_siderite"));
        command
            .args(args)
            .current_dir(dir)
            .env("CARGO_TARGET_DIR", &self.target)
            .env_remove("DATABASE_URL")
            .env_remove("SIDERITE_DATABASES__DEFAULT__URL");
        command
    }

    /// Run `siderite ARGS` in the project; it must succeed.
    fn step(&self, args: &[&str]) -> Output {
        let output = self.command(&self.project, args).output().unwrap();
        let shown = args.join(" ");
        assert!(output.status.success(), "siderite {shown}: {output:?}");
        output
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.step(args);
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn edit(&self, rel: &str, from: &str, to: &str) {
        let path = self.project.join(rel);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{rel} has no `{from}`");
        std::fs::write(path, text.replacen(from, to, 1)).unwrap();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// `GET path` on `port`: the status line, once the server answers.
fn get(port: u16, path: &str, within: Duration) -> String {
    let started = Instant::now();
    loop {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let request = format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\n\
                 Connection: close\r\n\r\n"
            );
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            if let Some(line) = response.lines().next() {
                return line.to_owned();
            }
        }
        assert!(started.elapsed() < within, "no answer on {port}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Stop `siderite dev` the way a terminal does (Ctrl-C).
fn interrupt(mut child: Child) {
    let pid = child.id().to_string();
    let _ = Command::new("kill").args(["-INT", &pid]).status();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The resource of the journey, as generator arguments.
const FIELDS: [&str; 5] = [
    "book",
    "title:String", // required
    "pages:i32?",   // nullable
    "--table",
    "books",
];

#[cfg(unix)]
#[test]
#[ignore = "slow: builds and packages a generated project"]
fn the_journey_needs_no_direct_cargo_command() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().unwrap().parent().unwrap();
    let id = std::process::id();
    let scratch = std::env::temp_dir().join(format!("siderite-journey-{id}"));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let journey = Journey {
        project: scratch.join("shop"),
        target: repo.join("target"),
    };
    // 1. Prerequisites.
    let setup = journey.command(&scratch, &["setup", "--json"]).output();
    let setup = serde_json::from_slice(&setup.unwrap().stdout);
    let setup: serde_json::Value = setup.unwrap();
    assert_eq!(setup["ok"], true, "{setup:#}");
    // 2. Scaffold.
    let repo_arg = repo.display().to_string();
    let args = ["new", "shop", "--template", "api", "--path", &repo_arg];
    let made = journey.command(&scratch, &args).output().unwrap();
    assert!(made.status.success(), "{made:?}");
    let doctor = journey.json(&["doctor", "--json"]);
    assert_eq!(doctor["ok"], true, "{doctor:#}");
    // 3. Develop: `dev` builds, serves and stops on Ctrl-C.
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let dev = journey
        .command(&journey.project, &["dev"])
        .env("ADDR", &addr)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let live = get(port, "/livez", Duration::from_secs(600));
    interrupt(dev);
    assert!(live.contains(" 200 "), "{live}");
    // 4. A resource, registered as the generator says, and its migration.
    let mut resource = vec!["generate", "resource"];
    resource.extend(FIELDS);
    let printed = journey.step(&resource);
    let printed = String::from_utf8_lossy(&printed.stdout).into_owned();
    // The model's registration, pasted exactly as printed.
    let meta = printed.split("add `").find_map(|rest| {
        let (expr, tail) = rest.split_once('`')?;
        tail.starts_with(" to `AppCli::models").then_some(expr)
    });
    let meta = meta.unwrap_or_else(|| panic!("{printed}"));
    journey.edit(
        "src/lib.rs",
        "use siderite::prelude::*;\n",
        "use siderite::prelude::*;\n\npub mod models {\n    pub mod book;\n}\n\
         pub mod routes {\n    pub mod book;\n}\n",
    );
    journey.edit(
        "src/lib.rs",
        ".routes(routes![create_greeting])",
        ".routes(routes![create_greeting])\n        \
         .routes(routes::book::routes())",
    );
    let models = format!(".settings(settings)\n        .models(&[{meta}])");
    journey.edit("src/main.rs", ".settings(settings)", &models);
    journey.step(&["makemigrations"]);
    journey.step(&["migrate"]);
    let status = journey.json(&["--json", "showmigrations"]);
    assert_eq!(status["ok"], true, "{status:#}");
    // 5. Tests for it, AI context, validation.
    let mut tests = vec!["generate", "test", "resource"];
    tests.extend(FIELDS);
    journey.step(&tests);
    journey.step(&["ai", "init"]);
    journey.step(&["ai", "context", "--task", "add an author to books"]);
    let verified = journey.json(&["verify", "--json"]);
    assert_eq!(verified["ok"], true, "{verified:#}");
    // 6. Package (the build created Cargo.lock).
    let dist = scratch.join("dist");
    let dist_arg = dist.display().to_string();
    let args = ["package", "--release", "--output", &dist_arg];
    journey.step(&args);
    assert!(dist.join("manifest.json").is_file());
    assert!(dist.join("migrations").is_dir());
    let _ = std::fs::remove_dir_all(&scratch);
}
