//! Argument handling, presets and the Cargo commands they become.

#![allow(clippy::unwrap_used)]

use super::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

fn planned(list: &[&str]) -> Result<Vec<String>, CliError> {
    let request = parse(&args(list))?;
    let commands = commands(&request, "app")?;
    Ok(commands.iter().map(|c| c.join(" ")).collect())
}

#[test]
fn crate_edits_pass_through_for_the_named_package() {
    let add = planned(&["add", "serde", "--features", "derive"]).unwrap();
    assert_eq!(add, ["add --package app serde --features derive"]);
    let remove = planned(&["remove", "serde", "--dry-run"]).unwrap();
    assert_eq!(remove, ["remove --package app serde --dry-run"]);
}

#[test]
fn presets_become_feature_changes_on_an_existing_dependency() {
    let one = planned(&["add", "--preset", "postgres", "--dry-run"]);
    let one = one.unwrap();
    let want = "add --package app siderite-cli --features postgres --dry-run";
    assert_eq!(one, [want]);
    let two = planned(&["add", "--preset=mysql", "--preset", "postgres"]);
    assert_eq!(two.unwrap().len(), 2);
    let request = parse(&args(&["add", "--preset", "postgres"])).unwrap();
    assert!(require(&request, &["siderite-cli".to_owned()]).is_ok());
    let err = require(&request, &["siderite".to_owned()]).unwrap_err();
    assert!(
        err.to_string().contains("siderite add siderite-cli"),
        "{err}"
    );
}

#[test]
fn preset_removal_is_described_not_guessed() {
    let err = planned(&["remove", "--preset", "postgres"]).unwrap_err();
    assert!(matches!(err, CliError::Conflict(_)), "{err}");
    let text = err.to_string();
    assert!(text.contains("remove \"postgres\" from the `features` of"));
}

#[test]
fn usage_errors() {
    for bad in [
        &["add"][..],
        &["remove"],
        &["add", "--dry-run"],
        &["add", "--preset"],
        &["add", "--preset", "redis"],
        &["add", "--preset", "postgres", "serde"],
        &["add", "--preset", "postgres", "--features", "x"],
        &["frob", "serde"],
    ] {
        let err = parse(&args(bad)).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{bad:?}");
    }
    let unknown = parse(&args(&["add", "--preset", "redis"])).unwrap_err();
    assert!(unknown.to_string().contains("presets: mysql, postgres"));
}
