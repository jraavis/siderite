use super::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

#[test]
fn parses_flags() {
    let parsed = parse(&args(&["verify", "--live", "--json"]));
    let want = Options {
        json: true,
        live: true,
    };
    assert_eq!(parsed.ok(), Some(want));
    assert!(parse(&args(&["verify", "--fix"])).is_err());
}

#[test]
fn check_runs_the_selected_binary() {
    let all = plans(false, Some("api"));
    let names: Vec<&str> = all.iter().map(|p| p.name).collect();
    assert_eq!(names, ["fmt", "lint", "build", "test", "check"]);
    let check = &all[4].args;
    assert_eq!(
        check,
        &args(&["run", "--bin", "api", "--", "check", "--json"])
    );
    assert!(all[3].needs_build && all[4].needs_build);
    let lint = cargo_args(&all[1]);
    let want = [
        "clippy",
        "--all-targets",
        JSON_FORMAT,
        "--",
        "-D",
        "warnings",
    ];
    assert_eq!(lint, args(&want));
    assert_eq!(
        cargo_args(&all[2]),
        args(&["build", "--all-targets", JSON_FORMAT])
    );
    assert_eq!(cargo_args(&all[0]), args(&["fmt", "--check"]));
}

#[test]
fn tail_keeps_the_last_lines_redacted() {
    let text: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let kept = tail(&format!("{text}url postgres://u:pw@h/db\n"));
    assert_eq!(kept.lines().count(), TAIL);
    assert!(kept.starts_with("line 11"), "{kept}");
    assert!(!kept.contains(":pw@"), "{kept}");
}

#[test]
fn reports_match_the_schema() {
    let pending = Pending {
        name: "build",
        command: "cargo build".into(),
        started: Instant::now(),
    };
    let failed = finish(pending, Some(101), text("e".into()));
    let steps = vec![failed.clone(), skipped("test", "cargo test".into())];
    let data = VerifyData {
        profile: "live",
        diagnostic_format: DIAGNOSTIC_FORMAT,
        test_results: "aggregate",
        steps,
    };
    let report = Envelope::new("verify", data, vec![failure(&failed)]);
    assert_eq!(report.exit_code(), 1);
    let value = serde_json::to_value(&report).unwrap_or_default();
    crate::fixtures::assert_schema_valid(&value);
    let text = render(&report);
    assert!(text.contains("FAILED   build"), "{text}");
    assert!(
        text.contains("verify (live): 1 of 2 steps failed"),
        "{text}"
    );
}

/// One compiler message line at `level` with `message`.
fn message(level: &str, message: &str) -> String {
    let rendered = format!("{level}: {message}\n");
    let value = serde_json::json!({
        "reason": "compiler-message",
        "message": {
            "level": level,
            "code": {"code": "E0308", "explanation": null},
            "message": message,
            "spans": [],
            "rendered": rendered,
        },
    });
    value.to_string()
}

#[test]
fn capture_keeps_distinct_diagnostics_errors_first() {
    let warn = message("warning", "unused");
    let error = message("error", "postgres://u:pw@h/db is wrong");
    let artifact = r#"{"reason":"compiler-artifact","target":{"name":"a"}}"#;
    let stdout = [warn.as_str(), &error, &warn, artifact, "test x ... ok"];
    let got = capture(&stdout.join("\n"), "error: aborting\n", true);
    let levels = got.diagnostics.iter().map(|d| d.level.as_str());
    assert_eq!(levels.collect::<Vec<_>>(), ["error", "warning"]);
    assert_eq!(got.omitted, 0);
    assert!(!got.diagnostics[0].message.contains(":pw@"));
    let rendered = got.diagnostics[0].rendered.as_deref().unwrap_or("");
    assert!(!rendered.contains(":pw@"), "{rendered}");
    assert_eq!(got.text, "error: aborting\ntest x ... ok\n");
    let plain = capture("out\n", "err\n", false);
    assert_eq!(
        (plain.text.as_str(), plain.diagnostics.len()),
        ("err\nout\n", 0)
    );
}

#[test]
fn capture_is_bounded() {
    let lines: Vec<String> = (0..capture::MAX_DIAGNOSTICS + 7)
        .map(|i| message("warning", &format!("w{i}")))
        .collect();
    let got = capture(&lines.join("\n"), "", true);
    assert_eq!(got.diagnostics.len(), capture::MAX_DIAGNOSTICS);
    assert_eq!(got.omitted, 7);
}
