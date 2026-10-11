//! Package arguments, declarations, file selection and replacement.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::fixtures::ScratchDir;
use serde_json::json;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

#[test]
fn arguments() {
    let cwd = Path::new("/work");
    let words = ["package", "--release", "--output", "dist", "--force"];
    let parsed = parse(&args(&words), cwd).unwrap();
    assert_eq!(parsed.output, Some(PathBuf::from("/work/dist")));
    assert!(parsed.force && !parsed.json);
    for bad in [
        &["package"][..],
        &["package", "--debug"],
        &["package", "--release", "--output"],
    ] {
        assert!(parse(&args(bad), cwd).is_err(), "{bad:?}");
    }
}

#[test]
fn declarations_and_the_migrations_default() {
    let dir = ScratchDir::new("package-declared");
    let none = declared(&Value::Null, dir.path()).unwrap();
    assert!(none.is_empty());
    std::fs::create_dir(dir.path().join("migrations")).unwrap();
    let meta = json!({ "package": { "assets": ["static", "templates"] } });
    let paths = declared(&meta, dir.path()).unwrap();
    assert_eq!(paths, ["static", "templates", "migrations"]);
    let custom = json!({ "package": { "migrations": "db/migrations" } });
    assert_eq!(declared(&custom, dir.path()).unwrap(), ["db/migrations"]);
    for bad in [
        json!({ "package": { "assets": "static" } }),
        json!({ "package": { "assets": [1] } }),
        json!({ "package": { "migrations": false } }),
    ] {
        assert!(declared(&bad, dir.path()).is_err(), "{bad}");
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn selection_leaves_out_secrets_databases_and_caches() {
    let dir = ScratchDir::new("package-select");
    let root = dir.path();
    write(root, "static/app.js", "console.log(1)");
    write(root, "static/css/site.css", "body{}");
    write(root, "static/.env", "SECRET=1");
    write(root, "static/cache.db", "x");
    write(root, "static/.git/HEAD", "ref");
    write(root, "static/tls/server.key", "k");
    write(root, "migrations/0001_initial.json", "{}");
    let declared = args(&["static", "migrations", "static/app.js"]);
    let picked = files::select(root, &declared).unwrap();
    let rels = picked.files.iter().map(|f| f.rel.as_str());
    let rels: Vec<&str> = rels.collect();
    let want = [
        "migrations/0001_initial.json",
        "static/app.js",
        "static/css/site.css",
    ];
    assert_eq!(rels, want);
    let left = [
        "static/.env",
        "static/.git",
        "static/cache.db",
        "static/tls/server.key",
    ];
    assert_eq!(picked.excluded, left);
    assert!(files::credentials(&picked.files).is_none());
    write(
        root,
        "static/config.js",
        "url = 'postgres://u:hunter2@h/db'",
    );
    let picked = files::select(root, &args(&["static"])).unwrap();
    let found = files::credentials(&picked.files);
    assert_eq!(found.as_deref(), Some("static/config.js"));
    for bad in [&["../outside"][..], &["/etc"], &["missing"], &[""]] {
        assert!(files::select(root, &args(bad)).is_err(), "{bad:?}");
    }
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_refused() {
    let dir = ScratchDir::new("package-links");
    write(dir.path(), "static/a.txt", "a");
    let link = dir.path().join("static/b.txt");
    std::os::unix::fs::symlink("/etc/passwd", link).unwrap();
    let err = files::select(dir.path(), &args(&["static"])).unwrap_err();
    assert!(err.to_string().contains("symbolic link"), "{err}");
}

#[test]
fn only_an_earlier_package_is_replaced_and_only_with_force() {
    let dir = ScratchDir::new("package-room");
    let out = dir.path().join("out");
    make_room(&out, false).unwrap();
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("notes.txt"), "mine").unwrap();
    let err = make_room(&out, true).unwrap_err();
    assert!(err.to_string().contains("not a siderite package"), "{err}");
    std::fs::write(out.join(MANIFEST), "{}").unwrap();
    let err = make_room(&out, false).unwrap_err();
    assert!(err.to_string().contains("--force"), "{err}");
    make_room(&out, true).unwrap();
    assert!(!out.exists());
}

#[test]
fn copies_are_checksummed() {
    let dir = ScratchDir::new("package-copy");
    let from = dir.path().join("a");
    std::fs::write(&from, "abc").unwrap();
    let entry = files::copy(&from, &dir.path().join("x/a"), "x/a").unwrap();
    let abc = "ba7816bf8f01cfea414140de5dae2223\
               b00361a396177a9cb410ff61f20015ad";
    assert_eq!((entry.bytes, entry.sha256.as_str()), (3, abc));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("x/a")).unwrap(),
        "abc"
    );
}
