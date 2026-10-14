//! Watch inputs, filtering and debouncing, with real filesystem events.

#![allow(clippy::unwrap_used)]

use super::watch::{Inputs, Layout, Watch, dep_info, relevant};
use super::*;
use crate::fixtures::{ScratchDir, block_on};
use std::path::Path;

fn layout(root: &Path) -> Layout {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::create_dir_all(root.join("migrations")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    Layout {
        package: root.to_path_buf(),
        root: root.to_path_buf(),
        target: root.join("target"),
        migrations: root.join("migrations"),
    }
}

#[test]
fn dep_info_paths_with_escapes_and_continuations() {
    let text = "/t/debug/app: /p/src/main.rs /p/my\\ dir/a.rs \\\n /p/b.rs\n\
                /p/src/main.rs:\n";
    let paths = dep_info(text);
    let want = ["/p/src/main.rs", "/p/my dir/a.rs", "/p/b.rs"];
    let got: Vec<_> = paths.iter().map(|p| p.to_str().unwrap()).collect();
    assert_eq!(got, want);
    assert!(dep_info("").is_empty());
}

#[test]
fn inputs_exclude_target_and_vcs_inside_the_workspace() {
    // A `target` directory *above* the workspace must not hide it.
    let scratch = ScratchDir::new("watch-inputs");
    let root = scratch.path().join("target/project");
    let layout = layout(&root);
    let sources = vec![
        root.join("src/main.rs"),
        root.join("target/debug/build/out.rs"),
        root.join(".git/hooks/x.rs"),
    ];
    let inputs = Inputs::new(&layout, sources);
    assert!(inputs.matches(&root.join("src/main.rs")));
    assert!(inputs.matches(&root.join("Cargo.toml")));
    assert!(inputs.matches(&root.join("migrations/0001_x.json")));
    assert!(!inputs.matches(&root.join("target/debug/build/out.rs")));
    assert!(!inputs.matches(&root.join(".git/hooks/x.rs")));
    assert!(!inputs.matches(&root.join("app.db")));
    let dirs = inputs.watch_dirs();
    assert!(dirs.iter().all(|d| !d.starts_with(root.join("target"))));
}

/// A workspace reached through a link (macOS's `/var` is one) still
/// ignores a missing file several directories deep under `target`.
#[cfg(unix)]
#[test]
fn target_is_ignored_through_a_linked_workspace() {
    let scratch = ScratchDir::new("watch-linked");
    let real = scratch.path().join("real");
    layout(&real);
    let linked = scratch.path().join("linked");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    let mut layout = layout(&linked);
    layout.target = real.join("target");
    let out = linked.join("target/debug/build/x/out/gen.rs");
    let sources = vec![linked.join("src/main.rs"), out.clone()];
    let inputs = Inputs::new(&layout, sources);
    assert!(inputs.matches(&linked.join("src/main.rs")));
    assert!(!inputs.matches(&out));
}

#[test]
fn overflow_and_errors_count_as_changes() {
    use notify::event::{AccessKind, Event as Raw, EventKind, Flag};
    let inputs = Inputs::default();
    let rescan = Raw::new(EventKind::Other).set_flag(Flag::Rescan);
    assert!(relevant(&Ok(rescan), &inputs));
    assert!(relevant(
        &Err(notify::Error::generic("queue overflow")),
        &inputs
    ));
    let kind = EventKind::Access(AccessKind::Any);
    let access = Raw::new(kind).add_path("/x".into());
    assert!(!relevant(&Ok(access), &inputs));
}

/// The next debounced change within `within`, if any.
async fn next(watch: &mut Watch, inputs: &Inputs, within: Duration) -> bool {
    tokio::time::timeout(within, watch.changed(inputs))
        .await
        .is_ok()
}

#[test]
fn real_events_are_filtered_and_debounced() {
    let scratch = ScratchDir::new("watch-events");
    let root = scratch.path().to_path_buf();
    let layout = layout(&root);
    let inputs = Inputs::initial(&layout);
    block_on(async {
        let mut watch = Watch::start().unwrap();
        watch.set(&inputs);
        let quiet = Duration::from_millis(600);
        // Files the app writes next to its sources are not inputs.
        for i in 0..20 {
            std::fs::write(root.join("app.db"), format!("{i}")).unwrap();
            std::fs::write(root.join("target/debug/app"), "x").unwrap();
        }
        assert!(!next(&mut watch, &inputs, quiet).await);
        // An editor-style save: write a temp file, rename over the input.
        let tmp = root.join("src/.main.rs.tmp");
        std::fs::write(&tmp, "fn main() { }\n").unwrap();
        std::fs::rename(&tmp, root.join("src/main.rs")).unwrap();
        assert!(next(&mut watch, &inputs, Duration::from_secs(5)).await);
        // A burst of saves is one change.
        for i in 0..20 {
            let text = format!("fn main() {{ let _ = {i}; }}\n");
            std::fs::write(root.join("src/main.rs"), text).unwrap();
        }
        assert!(next(&mut watch, &inputs, Duration::from_secs(5)).await);
        assert!(!next(&mut watch, &inputs, quiet).await);
        // A busy, irrelevant file cannot hold a change back past the cap.
        let busy = root.join("app.db");
        let writer = tokio::spawn(async move {
            for i in 0..200 {
                let _ = std::fs::write(&busy, format!("{i}"));
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        std::fs::write(root.join("migrations/0001_x.json"), "{}").unwrap();
        let started = std::time::Instant::now();
        assert!(next(&mut watch, &inputs, Duration::from_secs(5)).await);
        assert!(started.elapsed() < Duration::from_millis(1_500));
        writer.abort();
    });
}
