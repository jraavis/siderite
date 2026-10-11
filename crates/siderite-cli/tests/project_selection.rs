//! `siderite` project selection against workspaces built on disk.
//!
//! Only `cargo metadata` runs (no build, no network), in a directory
//! whose name contains a space.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A temp directory removed on drop.
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("siderite sel {id}-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

fn siderite_path() -> String {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    crates.join("siderite").display().to_string()
}

fn write_package(dir: &Path, name: &str, bins: &[&str]) {
    let dep = format!("siderite = {{ path = \"{}\" }}", siderite_path());
    write_with_deps(dir, name, &format!("[dependencies]\n{dep}\n"), bins);
}

/// A package whose manifest ends with `deps` (raw TOML).
fn write_with_deps(dir: &Path, name: &str, deps: &str, bins: &[&str]) {
    fs::create_dir_all(dir.join("src")).unwrap();
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\
         edition = \"2024\"\n\n{deps}"
    );
    fs::write(dir.join("Cargo.toml"), manifest).unwrap();
    fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    for bin in bins {
        fs::create_dir_all(dir.join("src/bin")).unwrap();
        let file = dir.join("src/bin").join(format!("{bin}.rs"));
        fs::write(file, "fn main() {}\n").unwrap();
    }
}

fn virtual_workspace(root: &Path, members: &[&str]) {
    let quoted = members.iter().map(|m| format!("\"{m}\""));
    let list: Vec<String> = quoted.collect();
    let text = format!(
        "[workspace]\nresolver = \"3\"\nmembers = [{}]\n",
        list.join(", ")
    );
    fs::write(root.join("Cargo.toml"), text).unwrap();
}

fn siderite(cwd: &Path, args: &[&str], path: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_siderite"));
    command.args(args).current_dir(cwd);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command.output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn virtual_workspace_with_two_apps_is_ambiguous() {
    let root = scratch("two");
    write_package(&root.join("crates/api"), "api", &[]);
    write_package(&root.join("crates/worker"), "worker", &[]);
    virtual_workspace(&root, &["crates/api", "crates/worker"]);
    let out = siderite(&root, &["routes"], None);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(text.contains("api ("), "{text}");
    assert!(text.contains("worker ("), "{text}");
    assert!(text.contains("--package"), "{text}");
    assert!(text.contains("siderite sel"), "{text}");
}

#[test]
fn unknown_package_and_manifest_are_usage_errors() {
    let root = scratch("flags");
    write_package(&root.join("crates/api"), "api", &[]);
    virtual_workspace(&root, &["crates/api"]);
    let out = siderite(&root, &["-p", "ghost", "routes"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("members: api"), "{}", stderr(&out));
    let args = ["--manifest-path", "none.toml", "check"];
    let out = siderite(&root, &args, None);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("no such file"), "{}", stderr(&out));
}

#[test]
fn several_binaries_need_a_choice() {
    let root = scratch("bins");
    let app = root.join("app");
    write_package(&app, "app", &["extra"]);
    let out = siderite(&app, &["routes"], None);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(text.contains("app, extra"), "{text}");
    assert!(text.contains("--bin NAME"), "{text}");
    let out = siderite(&app, &["--bin", "nope", "routes"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("no binary `nope`"));
}

#[test]
fn virtual_workspace_without_an_app_keeps_the_old_message() {
    let root = scratch("none");
    fs::create_dir_all(root.join("lib/src")).unwrap();
    fs::write(root.join("lib/src/lib.rs"), "").unwrap();
    fs::write(
        root.join("lib/Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    virtual_workspace(&root, &["lib"]);
    let out = siderite(&root, &["routes"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("needs an application"));
}

#[test]
fn missing_cargo_is_a_clear_runtime_error() {
    let root = scratch("nocargo");
    write_package(&root.join("app"), "app", &[]);
    let empty = root.join("empty path");
    fs::create_dir_all(&empty).unwrap();
    let path = empty.display().to_string();
    let out = siderite(&root.join("app"), &["routes"], Some(&path));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("not found on PATH"),
        "{}",
        stderr(&out)
    );
}

/// A `cargo` on PATH that logs `cwd<TAB>arg<TAB>arg...` per call and
/// delegates `metadata` to the real cargo; everything else is a no-op.
struct FakeCargo {
    dir: Scratch,
    log: PathBuf,
}

#[cfg(unix)]
impl FakeCargo {
    fn new(name: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch(name);
        let real = which_cargo();
        let log = dir.join("calls.log");
        let script = format!(
            "#!/bin/sh\nprintf '%s' \"$(pwd -P)\" >> '{log}'\n\
             for a in \"$@\"; do printf '\\t%s' \"$a\" >> '{log}'; done\n\
             printf '\\n' >> '{log}'\n\
             if [ \"$1\" = metadata ]; then exec '{real}' \"$@\"; fi\n\
             exit 0\n",
            log = log.display(),
            real = real.display()
        );
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let path = bin.join("cargo");
        fs::write(&path, script).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        Self { dir, log }
    }

    fn path(&self) -> String {
        let old = std::env::var("PATH").unwrap_or_default();
        format!("{}:{old}", self.dir.join("bin").display())
    }

    /// Calls as `(cwd, args)`.
    fn calls(&self) -> Vec<(PathBuf, Vec<String>)> {
        let text = fs::read_to_string(&self.log).unwrap_or_default();
        let lines = text.lines().map(|l| {
            let mut parts = l.split('\t').map(str::to_owned);
            let cwd = PathBuf::from(parts.next().unwrap());
            (cwd, parts.collect())
        });
        lines.collect()
    }

    /// The single non-metadata call.
    fn run_call(&self) -> (PathBuf, Vec<String>) {
        let mut calls = self.calls();
        calls.retain(|(_, a)| a[0] != "metadata");
        assert_eq!(calls.len(), 1, "{calls:?}");
        calls.remove(0)
    }

    fn metadata_calls(&self) -> usize {
        let all = self.calls();
        let meta = all.iter().filter(|(_, a)| a[0] == "metadata");
        meta.count()
    }
}

#[cfg(unix)]
fn which_cargo() -> PathBuf {
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .map(|d| Path::new(d).join("cargo"))
        .find(|p| p.is_file())
        .unwrap()
}

#[cfg(unix)]
fn canon(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

#[cfg(unix)]
fn strs(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

#[cfg(unix)]
#[test]
fn simple_package_skips_cargo_metadata() {
    let root = scratch("fast");
    let app = root.join("app");
    write_package(&app, "app", &[]);
    let sub = app.join("src");
    let fake = FakeCargo::new("fast-fake");
    let out = siderite(&sub, &["routes", "--addr", "x y"], Some(&fake.path()));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(fake.metadata_calls(), 0);
    let (cwd, args) = fake.run_call();
    assert_eq!(cwd, canon(&app));
    assert_eq!(args, strs(&["run", "--", "routes", "--addr", "x y"]));
}

#[cfg(unix)]
#[test]
fn several_binaries_use_metadata_and_forward_bin() {
    let root = scratch("multi");
    let app = root.join("app");
    write_package(&app, "app", &["extra"]);
    let fake = FakeCargo::new("multi-fake");
    let args = ["--bin", "extra", "check", "--flag"];
    let out = siderite(&app, &args, Some(&fake.path()));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(fake.metadata_calls(), 1);
    let (cwd, got) = fake.run_call();
    assert_eq!(cwd, canon(&app));
    let want = ["run", "--bin", "extra", "--", "check", "--flag"];
    assert_eq!(got, strs(&want));
}

#[cfg(unix)]
#[test]
fn build_runs_in_the_package_found_from_a_workspace_root() {
    let root = scratch("build");
    write_package(&root.join("crates/api"), "api", &[]);
    write_package(&root.join("crates/lib"), "lib", &[]);
    fs::write(root.join("crates/lib/Cargo.toml"), plain("lib")).unwrap();
    virtual_workspace(&root, &["crates/api", "crates/lib"]);
    let fake = FakeCargo::new("build-fake");
    let out = siderite(&root, &["build", "--release"], Some(&fake.path()));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let (cwd, got) = fake.run_call();
    assert_eq!(cwd, canon(&root.join("crates/api")));
    assert_eq!(got, strs(&["build", "--release"]));
}

#[cfg(unix)]
fn plain(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n")
}

/// Run `routes` from a virtual workspace holding `a` (the given deps)
/// and `b` (no siderite dependency); the chosen directory, or the
/// usage error text when no application was found.
#[cfg(unix)]
fn pick_a(name: &str, deps_a: &str, root_extra: &str) -> Option<PathBuf> {
    let root = scratch(name);
    write_with_deps(&root.join("a"), "a", deps_a, &[]);
    write_with_deps(&root.join("b"), "b", "", &[]);
    let members = "[workspace]\nresolver = \"3\"\nmembers = [\"a\", \"b\"]\n";
    let text = format!("{members}{root_extra}");
    fs::write(root.join("Cargo.toml"), text).unwrap();
    let fake = FakeCargo::new(&format!("{name}-fake"));
    let out = siderite(&root, &["routes"], Some(&fake.path()));
    if out.status.code() == Some(2) {
        assert!(stderr(&out).contains("needs an application"));
        return None;
    }
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let (cwd, _) = fake.run_call();
    Some(cwd.strip_prefix(canon(&root)).unwrap().to_path_buf())
}

#[cfg(unix)]
#[test]
fn dev_and_build_only_dependencies_do_not_make_an_app() {
    let dep = format!("path = \"{}\"", siderite_path());
    let dev = format!("[dev-dependencies]\nsiderite = {{ {dep} }}\n");
    assert_eq!(pick_a("dev", &dev, ""), None);
    let build = format!("[build-dependencies]\nsiderite = {{ {dep} }}\n");
    assert_eq!(pick_a("build-dep", &build, ""), None);
    let normal = format!("[dependencies]\nsiderite = {{ {dep} }}\n");
    assert_eq!(pick_a("normal", &normal, ""), Some("a".into()));
}

#[cfg(unix)]
#[test]
fn renamed_and_workspace_inherited_dependencies_count() {
    let path = siderite_path();
    let dep = format!("package = \"siderite\", path = \"{path}\"");
    let renamed = format!("[dependencies]\nweb = {{ {dep} }}\n");
    assert_eq!(pick_a("renamed", &renamed, ""), Some("a".into()));
    let inherited = "[dependencies]\nsiderite.workspace = true\n";
    let table = "[workspace.dependencies]";
    let extra = format!("{table}\nsiderite = {{ path = \"{path}\" }}\n");
    assert_eq!(pick_a("inherit", inherited, &extra), Some("a".into()));
}

#[cfg(unix)]
#[test]
fn manifest_path_at_a_workspace_root_or_directory() {
    let root = scratch("manifest");
    write_package(&root.join("crates/api"), "api", &[]);
    write_package(&root.join("tools/cli"), "cli", &[]);
    fs::write(root.join("tools/cli/Cargo.toml"), plain("cli")).unwrap();
    virtual_workspace(&root, &["crates/api", "tools/cli"]);
    let elsewhere = scratch("manifest-cwd");
    for given in [root.join("Cargo.toml"), root.0.clone()] {
        let fake = FakeCargo::new("manifest-fake");
        let given = given.display().to_string();
        let args = ["--manifest-path", &given, "routes"];
        let out = siderite(&elsewhere, &args, Some(&fake.path()));
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        let (cwd, _) = fake.run_call();
        assert_eq!(cwd, canon(&root.join("crates/api")));
    }
}

#[test]
fn standalone_mode_ignores_a_broken_manifest_above() {
    let root = scratch("broken");
    fs::write(root.join("Cargo.toml"), "[workspace\nmembers = [").unwrap();
    let sub = root.join("work");
    fs::create_dir_all(&sub).unwrap();
    let args = ["migrate", "--database-url", "sqlite::memory:"];
    let out = siderite(&sub, &args, None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn standalone_mode_ignores_a_virtual_workspace_without_cargo() {
    let root = scratch("virt-nocargo");
    fs::create_dir_all(root.join("lib")).unwrap();
    virtual_workspace(&root, &["lib"]);
    let sub = root.join("work");
    fs::create_dir_all(&sub).unwrap();
    let empty = root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let path = empty.display().to_string();
    let args = ["migrate", "--database-url", "sqlite::memory:"];
    let out = siderite(&sub, &args, Some(&path));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn project_flags_before_new_are_rejected() {
    let root = scratch("new");
    let out = siderite(&root, &["-p", "api", "new", "demo"], None);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(!root.join("demo").exists());
    let out = siderite(&root, &["--bin=x", "new", "demo"], None);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn inline_short_package_flag_is_accepted() {
    let root = scratch("inline");
    write_package(&root.join("crates/api"), "api", &[]);
    virtual_workspace(&root, &["crates/api"]);
    let out = siderite(&root, &["-p=ghost", "routes"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("members: api"), "{}", stderr(&out));
}
