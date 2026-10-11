//! `doctor --live` probes.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::fixtures::{ScratchDir, block_on};
use siderite_migrations::{Migration, MigrationGraph, Migrator, Operation};
use std::path::Path;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

#[test]
fn live_options_parse_and_need_live() {
    let parsed = parse(&args(&["doctor", "--json"])).unwrap();
    assert!(parsed.json && parsed.live.is_none());
    let list = [
        "doctor",
        "--live",
        "--database",
        "a",
        "--database=b",
        "--timeout-ms=250",
    ];
    let live = parse(&args(&list)).unwrap().live.unwrap();
    assert_eq!(live.aliases, ["a", "b"]);
    assert_eq!(live.timeout, Duration::from_millis(250));
    for bad in [
        ["doctor", "--database", "a"].as_slice(),
        &["doctor", "--live", "--timeout-ms", "0"],
        &["doctor", "--live", "--timeout-ms"],
        &["doctor", "--frobnicate"],
    ] {
        assert_eq!(parse(&args(bad)).unwrap_err().exit_code(), 2, "{bad:?}");
    }
}

/// A project whose `siderite.toml` holds `toml`.
fn project(tag: &str, toml: &str) -> ScratchDir {
    let dir = ScratchDir::new(tag);
    std::fs::write(dir.path().join("siderite.toml"), toml).unwrap();
    dir
}

fn run_live(dir: &Path, live: Live) -> Envelope<DoctorData> {
    let probe = Probe {
        path: OsString::new(),
        cwd: dir.to_path_buf(),
        project: ProjectArgs::default(),
        process_env: false,
    };
    block_on(diagnose(&probe, Some(&live)))
}

fn live(aliases: &[&str], ms: u64) -> Live {
    Live {
        aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
        timeout: Duration::from_millis(ms),
        migrations_dir: PathBuf::from("migrations"),
    }
}

/// Codes of the live diagnostics (the offline ones depend on the host).
fn live_codes(report: &Envelope<DoctorData>) -> Vec<&str> {
    let is_live = |l: &str| l.starts_with("live");
    let live = |d: &&Diagnostic| d.location().is_some_and(is_live);
    report
        .diagnostics
        .iter()
        .filter(live)
        .map(|d| d.code())
        .collect()
}

fn row<'a>(report: &'a Envelope<DoctorData>, name: &str) -> &'a DoctorCheck {
    let found = report.data.checks.iter().find(|c| c.name == name);
    found.unwrap_or_else(|| panic!("no {name}: {report:?}"))
}

/// `app.db` with one applied migration, and its file.
fn applied_sqlite(dir: &Path) {
    let op = Operation::RunSQL {
        sql: "CREATE TABLE t (id INTEGER)".into(),
        reverse_sql: None,
    };
    let m = Migration::new("0001_t", vec![], vec![op], true, vec![]).unwrap();
    siderite_migrations::write_migration(&dir.join("migrations"), &m).unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.join("app.db").display());
    block_on(async {
        let db = crate::connect::connect_url(&url).await.unwrap();
        let graph = MigrationGraph::build(vec![m]).unwrap();
        Migrator::new(&db, &graph)
            .migrate(None, false)
            .await
            .unwrap();
    });
}

#[test]
fn sqlite_is_read_and_never_created() {
    let toml = "[databases.default]\nurl = \"sqlite://app.db?mode=rwc\"\n\
                [databases.gone]\nurl = \"sqlite://gone.db?mode=rwc\"\n\
                [databases.mem]\nurl = \"sqlite::memory:\"\n\
                [databases.cache]\nurl = \"redis://127.0.0.1:1\"\n";
    let dir = project("doctor-live-sqlite", toml);
    applied_sqlite(dir.path());
    let before = std::fs::metadata(dir.path().join("app.db")).unwrap();
    let report = run_live(dir.path(), live(&[], 2_000));
    let ok = row(&report, "live.default");
    assert_eq!(ok.status, Status::Ok, "{ok:?}");
    assert!(ok.detail.contains("1 applied, 0 pending"), "{ok:?}");
    assert_eq!(row(&report, "live.gone").status, Status::Error);
    assert!(
        !dir.path().join("gone.db").exists(),
        "doctor created a file"
    );
    assert_eq!(row(&report, "live.mem").status, Status::Info);
    assert_eq!(row(&report, "live.cache").status, Status::Info);
    let after = std::fs::metadata(dir.path().join("app.db")).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
}

#[test]
fn only_requested_aliases_are_probed() {
    let toml = "[databases.default]\nurl = \"sqlite://app.db\"\n\
                [databases.gone]\nurl = \"sqlite://gone.db\"\n";
    let dir = project("doctor-live-requested", toml);
    applied_sqlite(dir.path());
    let report = run_live(dir.path(), live(&["default", "default"], 2_000));
    assert!(live_codes(&report).is_empty(), "{report:?}");
    let live_rows = report.data.checks.iter();
    let names: Vec<_> = live_rows
        .filter(|c| c.name.starts_with("live."))
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, ["live.default"]);
    let report = run_live(dir.path(), live(&["nope"], 2_000));
    assert_eq!(live_codes(&report), ["doctor.E008"]);
}

/// Silent servers: they accept connections, never answer, and record
/// the most connections open at once across all of them.
#[cfg(feature = "postgres")]
struct Silent {
    ports: Vec<u16>,
    peak: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(feature = "postgres")]
impl Silent {
    fn start(count: usize) -> Self {
        use std::io::Read;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        let open = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let mut ports = Vec::new();
        for _ in 0..count {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            ports.push(listener.local_addr().unwrap().port());
            let open = open.clone();
            let (peak, stop) = (peak.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    let now = open.fetch_add(1, SeqCst) + 1;
                    peak.fetch_max(now, SeqCst);
                    let open = open.clone();
                    std::thread::spawn(move || {
                        let _ = stream.set_nonblocking(false);
                        // Returns at EOF: the client gave up.
                        let _ = stream.read_to_end(&mut Vec::new());
                        open.fetch_sub(1, SeqCst);
                    });
                }
            });
        }
        Self { ports, peak, stop }
    }
}

#[cfg(feature = "postgres")]
impl Drop for Silent {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(feature = "postgres")]
#[test]
fn silent_servers_are_bounded_and_redacted() {
    let silent = Silent::start(6);
    let mut toml = String::new();
    for (i, port) in silent.ports.iter().enumerate() {
        toml.push_str(&format!(
            "[databases.db{i}]\n\
             url = \"postgres://u:hunter2@127.0.0.1:{port}/x\"\n"
        ));
    }
    let dir = project("doctor-live-silent", &toml);
    let started = std::time::Instant::now();
    let report = run_live(dir.path(), live(&[], 300));
    // Six probes, at most four at a time: at least two timeouts long.
    assert!(started.elapsed() >= Duration::from_millis(600));
    let peak = silent.peak.load(std::sync::atomic::Ordering::SeqCst);
    // Concurrent (at least two at once) but never above the cap.
    assert!((2..=live::CONCURRENCY).contains(&peak), "peak {peak}");
    assert_eq!(live_codes(&report), ["doctor.E007"; 6]);
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("hunter2"), "{json}");
    assert!(json.contains("timed out"), "{json}");
}

/// Live servers from the compose file: a probe passes, and a wrong
/// password fails with a redacted message.
fn live_server(var: &str) {
    let Ok(url) = std::env::var(var) else {
        panic!("{var} is required for this live test");
    };
    let (scheme, rest) = url.split_once("://").unwrap();
    let (userinfo, host) = rest.rsplit_once('@').unwrap();
    let user = userinfo.split(':').next().unwrap();
    let wrong = format!("{scheme}://{user}:hunter2wrong@{host}");
    let toml = format!(
        "[databases.good]\nurl = \"{url}\"\n\
         [databases.bad]\nurl = \"{wrong}\"\n"
    );
    let dir = project("doctor-live-server", &toml);
    let report = run_live(dir.path(), live(&[], 5_000));
    assert_eq!(row(&report, "live.good").status, Status::Ok);
    assert_eq!(row(&report, "live.bad").status, Status::Error);
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("hunter2wrong"), "{json}");
}

#[test]
#[ignore = "live PostgreSQL; set DATABASE_URL"]
fn live_postgres_probe() {
    live_server("DATABASE_URL");
}

#[test]
#[ignore = "live MySQL; set MYSQL_URL"]
fn live_mysql_probe() {
    live_server("MYSQL_URL");
}
