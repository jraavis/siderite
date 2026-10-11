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
}
