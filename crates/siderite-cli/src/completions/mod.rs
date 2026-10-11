//! `siderite completions bash|zsh|fish`: shell completion scripts derived
//! from the command contract.
//!
//! Commands, subcommands (`openapi export`, `ai init`) and each
//! command's flags come from [`COMMANDS`] (flags are read from the
//! declared usage), so completions cannot drift from dispatch. The script
//! is printed to stdout; installing it is left to the user, so no shell
//! profile is ever edited.

use crate::commands::{COMMANDS, Command};
use crate::error::CliError;
use std::fmt::Write;

/// Flags accepted before any command.
const GLOBAL: &[&str] = &[
    "--manifest-path",
    "--package",
    "--bin",
    "--database",
    "--database-url",
    "--migrations-dir",
    "--addr",
    "--json",
    "--help",
];

/// Flags (and short forms) that take a value, so the word after them is
/// not a command.
const VALUED: &[&str] = &[
    "--manifest-path",
    "--package",
    "-p",
    "--bin",
    "--database",
    "--database-url",
    "--migrations-dir",
    "--addr",
    "--output",
    "-o",
    "--format",
    "--grace-ms",
    "--timeout-ms",
    "--max-bytes",
    "--name",
    "--steps",
    "--path",
    "--limit",
    "--task",
    "--budget",
    "--include",
];

/// Print the script for `shell` (`raw` starts at `completions`).
///
/// Errors:
///   A usage error for a missing or unknown shell.
pub(crate) fn run(raw: &[String]) -> Result<u8, CliError> {
    let shell = raw.iter().skip_while(|a| *a != "completions").nth(1);
    let script = match shell.map(String::as_str) {
        Some("bash") => bash(),
        Some("zsh") => zsh(),
        Some("fish") => fish(),
        _ => {
            let msg = "usage: completions bash|zsh|fish (prints the script; \
                       source it from your shell's startup file)";
            return Err(CliError::usage(msg));
        }
    };
    print!("{script}");
    Ok(0)
}

/// The command words, each once, in help order.
fn words() -> Vec<&'static str> {
    let mut words: Vec<&str> = Vec::new();
    for c in COMMANDS {
        if !words.contains(&c.word()) {
            words.push(c.word());
        }
    }
    words
}

/// The subcommands of `word` (`export` for `openapi`).
fn subcommands(word: &str) -> Vec<&'static str> {
    COMMANDS
        .iter()
        .filter(|c| c.word() == word)
        .filter_map(|c| c.name.split_once(' ').map(|(_, sub)| sub))
        .collect()
}

/// The long flags in `command`'s usage.
pub(crate) fn flags(command: &Command) -> Vec<&'static str> {
    let mut flags: Vec<&str> = command
        .usage
        .split(|c: char| c.is_whitespace() || "[]|.".contains(c))
        .filter(|token| token.starts_with("--") && token.len() > 2)
        .map(|token| token.split('=').next().unwrap_or(token))
        .collect();
    flags.dedup();
    flags
}

/// `(key, flags)` for every command: `word` or `word sub`.
fn flag_table() -> Vec<(&'static str, Vec<&'static str>)> {
    COMMANDS.iter().map(|c| (c.name, flags(c))).collect()
}

/// The bash script; `@VALUED@`, `@TOP@` and `@BRANCHES@` are filled in.
const BASH: &str = r#"# siderite bash completion: `siderite completions bash`.
_siderite() {
    local cur=${COMP_WORDS[COMP_CWORD]} cmd= sub= w i words=
    for ((i = 1; i < COMP_CWORD; i++)); do
        w=${COMP_WORDS[i]}
        case $w in
            @VALUED@) ((i++)) ;;
            -*) ;;
            *) if [[ -z $cmd ]]; then cmd=$w;
               elif [[ -z $sub ]]; then sub=$w; fi ;;
        esac
    done
    case $cmd in
        '') words="@TOP@" ;;
@BRANCHES@    esac
    COMPREPLY=($(compgen -W "$words" -- "$cur"))
}
complete -F _siderite siderite
"#;

fn bash() -> String {
    let mut branches = String::new();
    for word in words() {
        let _ = writeln!(branches, "        {word}) {} ;;", bash_branch(word));
    }
    let top = format!("{} {}", words().join(" "), GLOBAL.join(" "));
    BASH.replace("@VALUED@", &VALUED.join("|"))
        .replace("@TOP@", &top)
        .replace("@BRANCHES@", &branches)
}

/// The `words=...` assignment for `word` in the bash script.
fn bash_branch(word: &str) -> String {
    let subs = subcommands(word);
    let table = flag_table();
    let flags_of = |key: &str| {
        let found = table.iter().find(|(k, _)| *k == key);
        found.map_or_else(String::new, |(_, f)| f.join(" "))
    };
    if subs.is_empty() {
        return format!("words=\"{}\"", flags_of(word));
    }
    let mut branch = format!(
        "if [[ -z $sub ]]; then words=\"{}\"; else case $sub in",
        subs.join(" ")
    );
    for sub in subs {
        let key = format!("{word} {sub}");
        let _ = write!(branch, " {sub}) words=\"{}\" ;;", flags_of(&key));
    }
    branch.push_str(" esac; fi");
    branch
}

fn zsh() -> String {
    // Same logic as bash; `compadd` offers the words.
    let bash = bash();
    let body = bash
        .split_once("_siderite() {\n")
        .map_or("", |(_, rest)| rest)
        .split_once("    COMPREPLY=")
        .map_or("", |(body, _)| body)
        .replace("COMP_CWORD", "CURRENT")
        .replace("COMP_WORDS", "words")
        .replace("for ((i = 1; i < CURRENT", "for ((i = 2; i < CURRENT")
        .replace(" words=", " opts=")
        .replace("words=\"", "opts=\"");
    let mut out = String::from(
        "#compdef siderite\n\
         # siderite completion for zsh (`siderite completions zsh`).\n\
         _siderite() {\n",
    );
    out.push_str(&body);
    out.push_str(
        "    compadd -- ${=opts}\n}\n\
         if [[ ${zsh_eval_context[-1]} == loadautofunc ]]; then\n    \
         _siderite \"$@\"\nelse\n    compdef _siderite siderite\nfi\n",
    );
    out
}

fn fish() -> String {
    let mut out = String::from(
        "# siderite completion for fish (`siderite completions fish`).\n\
         complete -c siderite -f\n",
    );
    for c in COMMANDS.iter().filter(|c| !c.name.contains(' ')) {
        let _ = writeln!(
            out,
            "complete -c siderite -n __fish_use_subcommand -a {} -d {}",
            c.word(),
            quote(c.summary)
        );
    }
    for word in words() {
        let subs = subcommands(word);
        if subs.is_empty() {
            continue;
        }
        let summary = |sub: &str| {
            let name = format!("{word} {sub}");
            COMMANDS
                .iter()
                .find(|c| c.name == name)
                .map_or("", |c| c.summary)
        };
        let _ = writeln!(
            out,
            "complete -c siderite -n __fish_use_subcommand -a {word} -d {}",
            quote(&format!("{} commands", word.to_uppercase()))
        );
        for sub in &subs {
            let _ = writeln!(
                out,
                "complete -c siderite -n '__fish_seen_subcommand_from {word}; \
                 and not __fish_seen_subcommand_from {}' -a {sub} -d {}",
                subs.join(" "),
                quote(summary(sub))
            );
        }
    }
    for flag in GLOBAL {
        let long = flag.trim_start_matches("--");
        let _ = writeln!(
            out,
            "complete -c siderite -n __fish_use_subcommand -l {long}"
        );
    }
    for (key, flags) in flag_table() {
        let last = key.rsplit(' ').next().unwrap_or(key);
        for flag in flags {
            let long = flag.trim_start_matches("--");
            let seen = format!("'__fish_seen_subcommand_from {last}'");
            let _ = writeln!(out, "complete -c siderite -n {seen} -l {long}");
        }
    }
    out
}

/// A single-quoted fish string.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests;
