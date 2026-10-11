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
    let failed = finish(pending, Some(101), "e");
    let steps = vec![failed.clone(), skipped("test", "cargo test".into())];
    let data = VerifyData {
        profile: "live",
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
