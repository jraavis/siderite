#![allow(clippy::unwrap_used)]

use super::*;
use serde_json::json;

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

fn split(list: &[&str]) -> (ProjectArgs, Vec<String>) {
    split_project(&strings(list)).unwrap()
}

/// `(name, dir, uses siderite, bins, default-run)`.
type Spec<'a> = (&'a str, &'a str, bool, &'a [&'a str], Option<&'a str>);

fn ws(specs: &[Spec]) -> Workspace {
    let packages: Vec<_> = specs
        .iter()
        .map(|(name, dir, uses, bins, default_run)| {
            let deps = if *uses {
                vec![json!({"name": "siderite"})]
            } else {
                vec![]
            };
            let mut targets: Vec<_> = bins
                .iter()
                .map(|b| json!({"name": b, "kind": ["bin"]}))
                .collect();
            targets.push(json!({"name": name, "kind": ["lib"]}));
            json!({
                "name": name,
                "manifest_path": format!("{dir}/Cargo.toml"),
                "dependencies": deps,
                "targets": targets,
                "default_run": default_run,
            })
        })
        .collect();
    let text = json!({ "packages": packages }).to_string();
    workspace::parse(&text).unwrap()
}

type Picked<'a> = Result<Option<&'a str>, CliError>;

fn pick<'a>(ws: &'a Workspace, args: &ProjectArgs, cwd: &str) -> Picked<'a> {
    let found = select(ws, args, None, Path::new(cwd))?;
    Ok(found.map(|p| p.name.as_str()))
}

#[test]
fn flags_before_the_command_are_taken_out() {
    let (project, rest) = split(&[
        "--manifest-path",
        "my app/Cargo.toml",
        "-p",
        "api",
        "--bin=serve",
        "--addr",
        "0.0.0.0:1",
        "run",
        "--addr",
        "x",
    ]);
    assert_eq!(project.manifest_path, Some("my app/Cargo.toml".into()));
    assert_eq!(project.package.as_deref(), Some("api"));
    assert_eq!(project.bin.as_deref(), Some("serve"));
    assert_eq!(rest, ["--addr", "0.0.0.0:1", "run", "--addr", "x"]);
}

#[test]
fn app_arguments_are_forwarded_unchanged() {
    for tail in [
        &["run", "-p", "9", "--bin", "x"][..],
        &["run", "--", "-p", "--package=z"][..],
        &["migrate", "--manifest-path", "m"][..],
        &["--", "-p", "a"][..],
    ] {
        let (project, rest) = split(tail);
        assert_eq!(project, ProjectArgs::default(), "{tail:?}");
        assert_eq!(rest, tail);
    }
}

#[test]
fn flag_errors_are_usage_errors() {
    for bad in [
        &["--bin"][..],
        &["-p"][..],
        &["--package="][..],
        &["-p", "a", "--package", "b", "run"][..],
    ] {
        let err = split_project(&strings(bad)).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{bad:?}");
    }
}

#[test]
fn explicit_package_wins_over_the_cwd() {
    let ws = ws(&[
        ("api", "/w/api", true, &["api"], None),
        ("worker", "/w/worker", true, &["worker"], None),
    ]);
    let args = ProjectArgs {
        package: Some("worker".into()),
        ..ProjectArgs::default()
    };
    assert_eq!(pick(&ws, &args, "/w/api").unwrap(), Some("worker"));
    let args = ProjectArgs {
        package: Some("nope".into()),
        ..ProjectArgs::default()
    };
    let err = pick(&ws, &args, "/w").unwrap_err();
    assert!(err.to_string().contains("members: api, worker"), "{err}");
    assert_eq!(err.exit_code(), 2);
}

#[test]
fn nearest_nested_package_wins() {
    let ws = ws(&[
        ("outer", "/w", true, &["outer"], None),
        ("inner", "/w/tools/inner", false, &["inner"], None),
    ]);
    let none = ProjectArgs::default();
    assert_eq!(
        pick(&ws, &none, "/w/tools/inner/src").unwrap(),
        Some("inner")
    );
    assert_eq!(pick(&ws, &none, "/w/tools").unwrap(), Some("outer"));
    assert_eq!(pick(&ws, &none, "/w").unwrap(), Some("outer"));
}

#[test]
fn manifest_path_selects_its_package() {
    let ws = ws(&[
        ("a", "/my work/a", true, &["a"], None),
        ("b", "/my work/b", true, &["b"], None),
    ]);
    let manifest = Path::new("/my work/b/Cargo.toml");
    let none = ProjectArgs::default();
    let found = select(&ws, &none, Some(manifest), Path::new("/my work/a"));
    assert_eq!(found.unwrap().unwrap().name, "b");
}

#[test]
fn virtual_workspace_picks_the_only_app() {
    let ws = ws(&[
        ("models", "/w/models", false, &[], None),
        ("web", "/w/web", true, &["web"], None),
        ("lib-user", "/w/lib", true, &[], None),
    ]);
    let none = ProjectArgs::default();
    assert_eq!(pick(&ws, &none, "/w").unwrap(), Some("web"));
}

#[test]
fn virtual_workspace_without_an_app_is_none() {
    let ws = ws(&[("models", "/w/models", false, &["tool"], None)]);
    assert_eq!(pick(&ws, &ProjectArgs::default(), "/w").unwrap(), None);
}

#[test]
fn several_apps_are_ambiguous_and_listed_in_order() {
    let ws = ws(&[
        ("zeta", "/w/z dir", true, &["zeta"], None),
        ("alpha", "/w/a", true, &["alpha"], None),
        ("mid", "/w/m", true, &["mid"], None),
    ]);
    let err = pick(&ws, &ProjectArgs::default(), "/w").unwrap_err();
    assert_eq!(err.exit_code(), 2);
    let text = err.to_string();
    let (a, m, z) = (
        text.find("alpha (").unwrap(),
        text.find("mid (").unwrap(),
        text.find("zeta (/w/z dir)").unwrap(),
    );
    assert!(a < m && m < z, "{text}");
    assert!(text.contains("--package NAME"), "{text}");
}

fn pkg(bins: &[&str], default_run: Option<&str>) -> Package {
    ws(&[("p", "/w", true, bins, default_run)])
        .packages
        .remove(0)
}

#[test]
fn bin_resolution_rules() {
    let one = pkg(&["p"], None);
    assert_eq!(resolve_bin(&one, None, true).unwrap(), None);
    let two = pkg(&["a", "b"], None);
    let err = resolve_bin(&two, None, true).unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("a, b"), "{err}");
    assert!(err.to_string().contains("--bin NAME"), "{err}");
    // build/test cover every target, so several binaries are fine.
    assert_eq!(resolve_bin(&two, None, false).unwrap(), None);
    let default = pkg(&["a", "b"], Some("b"));
    assert_eq!(resolve_bin(&default, None, true).unwrap(), None);
    assert_eq!(
        resolve_bin(&two, Some("b"), true).unwrap().as_deref(),
        Some("b")
    );
    let err = resolve_bin(&two, Some("c"), false).unwrap_err();
    assert!(err.to_string().contains("no binary `c`"), "{err}");
    let none = pkg(&[], None);
    assert_eq!(resolve_bin(&none, None, true).unwrap_err().exit_code(), 2);
}

#[test]
fn parse_reads_real_metadata_shape() {
    let text = json!({
        "packages": [{
            "name": "demo",
            "manifest_path": "/tmp/my app/Cargo.toml",
            "default_run": "demo",
            "dependencies": [{"name": "serde"}, {"name": "siderite"}],
            "targets": [
                {"name": "demo", "kind": ["bin"]},
                {"name": "demo", "kind": ["lib"]},
                {"name": "ex", "kind": ["example"]}
            ]
        }],
        "workspace_members": []
    })
    .to_string();
    let ws = workspace::parse(&text).unwrap();
    let pkg = &ws.packages[0];
    assert!(pkg.uses_siderite);
    assert_eq!(pkg.bins, ["demo"]);
    assert_eq!(pkg.default_run.as_deref(), Some("demo"));
    assert_eq!(pkg.dir(), Path::new("/tmp/my app"));
    assert!(workspace::parse("{}").is_err());
    assert!(workspace::parse("nope").is_err());
}

#[test]
fn missing_cargo_is_an_io_error_not_a_panic() {
    let dir = std::env::temp_dir();
    let cargo = "siderite-no-such-cargo";
    let err = workspace::load(cargo, &dir, None).unwrap_err();
    assert_eq!(err.exit_code(), 1);
    assert!(err.to_string().contains("not found on PATH"), "{err}");
}
