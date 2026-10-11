//! The configuration explanation: sources, precedence and redaction.

#![allow(clippy::unwrap_used)]

use crate::*;
use serde_json::json;

#[allow(clippy::result_large_err)]
fn jail(f: impl FnOnce(&mut figment::Jail)) {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        f(jail);
        Ok(())
    });
}

fn find<'a>(entries: &'a [ConfigEntry], key: &str) -> &'a ConfigEntry {
    let found = entries.iter().find(|e| e.key == key);
    found.unwrap_or_else(|| panic!("no {key} in {entries:?}"))
}

#[test]
fn every_value_names_its_source() {
    jail(|jail| {
        let toml = "[app]\nname = \"blog\"\n\n[databases.default]\n\
                    url = \"postgres://app:hunter2@db/blog\
                    ?sslmode=require\"\n";
        jail.create_file("siderite.toml", toml).unwrap();
        jail.set_env("SIDERITE_SERVER__ADDR", "0.0.0.0:9000");
        jail.set_env("SIDERITE_SECRET_KEY", "s3cret");
        let entries = ConfigBuilder::new()
            .file_optional("siderite.toml")
            .env_prefix("SIDERITE_")
            .set("app.debug", true)
            .explain()
            .unwrap();
        let name = find(&entries, "app.name");
        assert_eq!(name.value, json!("blog"));
        assert_eq!(name.source.kind, SourceKind::File);
        assert!(
            name.source
                .detail
                .as_deref()
                .unwrap()
                .ends_with("siderite.toml")
        );
        let addr = find(&entries, "server.addr");
        assert_eq!(addr.source.kind, SourceKind::Environment);
        let variable = addr.source.detail.as_deref();
        assert_eq!(variable, Some("SIDERITE_SERVER__ADDR"));
        assert_eq!(
            find(&entries, "app.debug").source.kind,
            SourceKind::Override
        );
        let level = find(&entries, "log.level");
        assert_eq!(
            (level.value.clone(), level.source.kind),
            (json!("info"), SourceKind::Default)
        );
        let url = find(&entries, "databases.default.url");
        let shown = url.value.as_str().unwrap();
        assert!(shown.starts_with("postgres://app:***@db/blog"), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert_eq!(find(&entries, "secret_key").value, json!("***"));
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
    });
}

#[test]
fn the_explanation_matches_the_loaded_settings() {
    jail(|jail| {
        jail.set_env("DATABASE_URL", "sqlite://app.db");
        let builder = ConfigBuilder::new().env_prefix("SIDERITE_");
        let settings = builder.build().unwrap();
        let entries = builder.explain().unwrap();
        let url = find(&entries, "databases.default.url");
        let loaded = settings.databases["default"].url.expose();
        assert_eq!(url.value, json!(loaded));
        assert_eq!(url.source.detail.as_deref(), Some("DATABASE_URL"));
    });
}

#[test]
fn parse_errors_never_show_credentials() {
    jail(|jail| {
        let broken = "[databases.default]\n\
                      url = \"postgres://u:hunter2@h/db\n";
        jail.create_file("siderite.toml", broken).unwrap();
        let err = ConfigBuilder::new()
            .file("siderite.toml")
            .build()
            .unwrap_err()
            .to_string();
        assert!(!err.contains("hunter2"), "{err}");
    });
}
