//! Generated completion scripts, executed by real shells when present.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::fixtures::ScratchDir;
use std::process::Command;

fn has(shell: &str) -> bool {
    Command::new(shell)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Words a shell offers for the command line `typed` (ending in a space
/// means "complete the next word").
fn offered(shell: &str, script: &str, typed: &str) -> Vec<String> {
    let scratch = ScratchDir::new("completions");
    let file = scratch.path().join("script");
    std::fs::write(&file, script).unwrap();
    let words: Vec<&str> = typed.split(' ').collect();
    let quoted: Vec<String> = words.iter().map(|w| format!("'{w}'")).collect();
    let program = match shell {
        "bash" => format!(
            "source '{}'; COMP_WORDS=({}); COMP_CWORD={}; _siderite; \
             printf '%s\\n' \"${{COMPREPLY[@]}}\"",
            file.display(),
            quoted.join(" "),
            words.len() - 1
        ),
        "zsh" => format!(
            "compdef() {{ :; }}; compadd() {{ shift; print -l -- \"$@\"; }}; \
             source '{}'; words=({}); CURRENT={}; _siderite",
            file.display(),
            quoted.join(" "),
            words.len()
        ),
        _ => format!("source '{}'; complete -C '{typed}'", file.display()),
    };
    let out = Command::new(shell).arg("-c").arg(program).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut words: Vec<String> = text
        .lines()
        .map(|l| l.split('\t').next().unwrap_or("").to_owned())
        .filter(|w| !w.is_empty())
        .collect();
    words.sort();
    words
}

fn script(shell: &str) -> String {
    match shell {
        "bash" => bash(),
        "zsh" => zsh(),
        _ => fish(),
    }
}

#[test]
fn flags_come_from_the_usage() {
    let find = |name: &str| COMMANDS.iter().find(|c| c.name == name).unwrap();
    assert_eq!(flags(find("openapi export")), ["--output", "--force"]);
    assert_eq!(flags(find("doctor")), ["--json", "--live", "--database"]);
    assert!(flags(find("migrate")).contains(&"--dry-run"));
}

#[test]
fn every_shell_completes_commands_subcommands_and_flags() {
    for shell in ["bash", "zsh", "fish"] {
        if !has(shell) {
            eprintln!("{shell} is not installed; skipping its run");
            continue;
        }
        let script = script(shell);
        let top = offered(shell, &script, "siderite ");
        for word in ["new", "dev", "openapi", "ai", "migrate", "lint"] {
            assert!(top.contains(&word.to_owned()), "{shell}: {top:?}");
        }
        let ai = offered(shell, &script, "siderite ai ");
        assert_eq!(ai, ["dump-context", "init"], "{shell}");
        let init = offered(shell, &script, "siderite ai init --");
        assert_eq!(init, ["--check", "--dry-run", "--force"], "{shell}");
        let export = offered(
            shell,
            &script,
            "siderite --manifest-path x openapi export --",
        );
        assert_eq!(export, ["--force", "--output"], "{shell}");
        let check = offered(shell, &script, "siderite check --");
        assert_eq!(check, ["--json"], "{shell}");
    }
}

#[test]
fn unknown_shells_are_usage_errors() {
    let args = |l: &[&str]| -> Vec<String> {
        let owned = l.iter().map(|a| (*a).to_owned());
        owned.collect()
    };
    for bad in [["completions"].as_slice(), &["completions", "tcsh"]] {
        assert_eq!(run(&args(bad)).unwrap_err().exit_code(), 2);
    }
}
