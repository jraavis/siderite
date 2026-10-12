//! Write plans against real directories.

#![allow(clippy::unwrap_used)]

use super::plan::Action;
use super::*;
use crate::fixtures::ScratchDir;

fn read(root: &std::path::Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

#[test]
fn creates_then_reports_unchanged_and_conflicts() {
    let scratch = ScratchDir::new("plan");
    let root = scratch.path();
    let mut plan = Plan::new(root);
    plan.add("src/a.rs", "fn a() {}\n").unwrap();
    plan.add("README.md", "# x\n").unwrap();
    let preview_text = preview(&plan.check().unwrap());
    assert!(preview_text.contains("create src/a.rs\n"), "{preview_text}");
    assert!(preview_text.contains("+fn a() {}\n"), "{preview_text}");
    assert!(!root.join("src").exists(), "check wrote something");
    plan.apply().unwrap();
    assert_eq!(read(root, "src/a.rs"), "fn a() {}\n");
    // Again: identical files are left alone.
    let again = plan.apply().unwrap();
    assert!(again.iter().all(|c| c.action == Action::Unchanged));
    // The user edits a file: a plain create now conflicts, nothing written.
    std::fs::write(root.join("src/a.rs"), "fn a() { mine() }\n").unwrap();
    let mut other = Plan::new(root);
    other.add("src/a.rs", "fn a() {}\n").unwrap();
    other.add("src/b.rs", "fn b() {}\n").unwrap();
    let err = other.apply().unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("src/a.rs"), "{err}");
    assert!(!root.join("src/b.rs").exists(), "partial write");
    assert_eq!(read(root, "src/a.rs"), "fn a() { mine() }\n");
    let text = preview(&other.check().unwrap());
    assert!(text.contains("conflict src/a.rs (kept)"), "{text}");
    assert!(text.contains("-fn a() { mine() }"), "{text}");
}

#[test]
fn owned_files_are_replaced_with_a_diff() {
    let scratch = ScratchDir::new("plan-owned");
    let root = scratch.path();
    std::fs::write(root.join("ctx.md"), "v1\n").unwrap();
    let mut plan = Plan::new(root);
    plan.add_owned("ctx.md", "v2\n".to_owned()).unwrap();
    let changes = plan.apply().unwrap();
    let Action::Replace { diff } = &changes[0].action else {
        panic!("{changes:?}");
    };
    assert!(diff.contains("-v1\n+v2\n"), "{diff}");
    assert_eq!(read(root, "ctx.md"), "v2\n");
}

#[test]
fn bad_paths_are_rejected() {
    let mut plan = Plan::new("/nowhere");
    for bad in ["", "/etc/passwd", "../x", "a/../../x", "./a"] {
        assert!(plan.add(bad, "").is_err(), "{bad}");
    }
    plan.add("a/b.rs", "").unwrap();
    for clash in ["a/b.rs", "a", "a/b.rs/c"] {
        assert!(plan.add(clash, "").is_err(), "{clash}");
    }
}

#[cfg(unix)]
#[test]
fn symbolic_links_cannot_redirect_writes() {
    let scratch = ScratchDir::new("plan-links");
    let outside = ScratchDir::new("plan-outside");
    let root = scratch.path();
    std::os::unix::fs::symlink(outside.path(), root.join("src")).unwrap();
    let mut plan = Plan::new(root);
    plan.add("src/evil.rs", "x").unwrap();
    assert!(
        plan.check()
            .unwrap_err()
            .to_string()
            .contains("symbolic link")
    );
    assert!(plan.apply().is_err());
    assert!(!outside.path().join("evil.rs").exists());
}

mod route_generator {
    use crate::fixtures::ScratchDir;
    use crate::generate::route::{file, instructions, parse, render};

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn the_output_is_the_compiled_golden_file() {
        let options = parse(&words(&["items"])).unwrap();
        assert_eq!(file(&options), "src/routes/items.rs");
        let golden = include_str!("../../tests/generated/route_items.rs");
        assert_eq!(crate::generate::rustfmt(&render(&options)), golden);
    }

    #[test]
    fn names_and_paths_are_checked() {
        let custom = parse(&words(&["blog_posts", "--path", "/api/posts"]));
        let custom = custom.unwrap();
        let text = render(&custom);
        assert!(text.contains("#[get(\"/api/posts\", tag = \"blog_posts\""));
        assert!(text.contains("pub struct BlogPostsInfo"), "{text}");
        for bad in [
            &[][..],
            &["Items"],
            &["type"],
            &["items", "extra"],
            &["items", "--path", "items"],
            &["items", "--path", "/a/{id}"],
            &["items", "--path", "/a\"b"],
            &["items", "--path", "/a//b"],
            &["items", "--method", "post"],
        ] {
            assert!(parse(&words(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn instructions_follow_the_crate_layout() {
        let scratch = ScratchDir::new("route-steps");
        let dir = scratch.path();
        let options = parse(&words(&["items"])).unwrap();
        let text = instructions(dir, &options);
        assert!(text.contains("pub mod items; }` to src/main.rs"), "{text}");
        std::fs::create_dir_all(dir.join("src/routes")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();
        std::fs::write(dir.join("src/routes/mod.rs"), "").unwrap();
        let text = instructions(dir, &options);
        assert!(
            text.contains("`pub mod items;` to src/routes/mod.rs"),
            "{text}"
        );
        assert!(text.contains(".routes(routes::items::routes())"), "{text}");
    }
}

mod model_generator {
    use crate::fixtures::ScratchDir;
    use crate::generate::model::{file, instructions, parse, render};

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn the_output_is_the_compiled_golden_file() {
        let spec = [
            "book",
            "title:String",
            "pages:i32?",
            "views:i64",
            "price:f64?",
            "in_stock:bool",
            "--table",
            "books",
        ];
        let options = parse(&words(&spec)).unwrap();
        assert_eq!(file(&options), "src/models/book.rs");
        let golden = include_str!("../../tests/generated/model_book.rs");
        assert_eq!(crate::generate::rustfmt(&render(&options)), golden);
    }

    #[test]
    fn names_types_and_fields_are_checked() {
        let bare = parse(&words(&["order_line"])).unwrap();
        assert_eq!(bare.table, "order_line");
        assert!(render(&bare).contains("pub struct OrderLine {"));
        for bad in [
            &[][..],
            &["Book"],
            &["book", "title"],
            &["book", "title:str"],
            &["book", "when:DateTime"],
            &["book", "id:i64"],
            &["book", "objects:i64"],
            &["book", "save:bool"],
            &["book", "a:i32", "a:i64"],
            &["book", "Title:String"],
            &["book", "--table", "Books"],
            &["book", "--force"],
        ] {
            assert!(parse(&words(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn instructions_name_the_registration() {
        let scratch = ScratchDir::new("model-steps");
        let dir = scratch.path();
        let options = parse(&words(&["book"])).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();
        let toml = "[package]\nname = \"my-shop\"\n";
        std::fs::write(dir.join("Cargo.toml"), toml).unwrap();
        let text = instructions(dir, &options);
        assert!(text.contains("pub mod book; }` to src/lib.rs"), "{text}");
        let meta = "`<my_shop::models::book::Book as siderite::orm::Model>\
                    ::META`";
        assert!(text.contains(meta), "{text}");
        assert!(text.contains("siderite makemigrations"), "{text}");
    }
}

mod resource_generator {
    use crate::fixtures::ScratchDir;
    use crate::generate::model::{self, parse};
    use crate::generate::resource::{file, instructions, render};

    fn options() -> model::ModelOptions {
        let spec = [
            "book",
            "title:String",
            "pages:i32?",
            "views:i64",
            "price:f64?",
            "in_stock:bool",
            "--table",
            "books",
        ];
        let words = spec.iter().map(|a| (*a).to_owned());
        parse(&words.collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn the_output_is_the_compiled_golden_file() {
        let options = options();
        assert_eq!(file(&options), "src/routes/book.rs");
        let golden = include_str!("../../tests/generated/resource_book.rs");
        let rustfmt = crate::generate::rustfmt;
        assert_eq!(rustfmt(&render(&options)), golden);
        // The model half is exactly what `generate model` writes.
        let model = include_str!("../../tests/generated/model_book.rs");
        assert_eq!(rustfmt(&model::render(&options)), model);
    }

    #[test]
    fn instructions_cover_model_and_routes() {
        let scratch = ScratchDir::new("resource-steps");
        let text = instructions(scratch.path(), &options());
        let meta = "`<models::book::Book as siderite::orm::Model>::META`";
        assert!(text.contains(meta), "{text}");
        assert!(text.contains(".routes(routes::book::routes())"), "{text}");
        assert!(text.contains("siderite migrate"), "{text}");
    }
}

mod tests_for_generated_code {
    use super::test::*;
    use crate::fixtures::ScratchDir;
    use crate::generate::rustfmt;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    fn resource() -> TestOptions {
        let spec = [
            "resource",
            "book",
            "title:String",
            "pages:i32?",
            "views:i64",
            "price:f64?",
            "in_stock:bool",
            "--table",
            "books",
        ];
        parse(&words(&spec)).unwrap()
    }

    #[test]
    fn the_outputs_are_the_compiled_golden_files() {
        let route = parse(&words(&["route", "items"])).unwrap();
        assert_eq!(file(&route), "tests/items.rs");
        let golden = include_str!("../../tests/generated/test_items.rs");
        assert_eq!(rustfmt(&render(&route, "crate")), golden);
        let resource = resource();
        assert_eq!(file(&resource), "tests/book.rs");
        let golden = include_str!("../../tests/generated/test_book.rs");
        assert_eq!(rustfmt(&render(&resource, "crate")), golden);
    }

    #[test]
    fn kinds_names_and_steps_are_checked() {
        for bad in [&[][..], &["handler", "x"], &["route", "Bad-Name"]] {
            assert!(parse(&words(bad)).is_err(), "{bad:?}");
        }
        let text = render(&resource(), "blog_api");
        assert!(text.contains("use blog_api::models::book::Book;"));
        assert!(text.contains("blog_api::app()"));
        // Only nullable fields: the `id` body is the one failure case.
        let optional = ["resource", "note", "body:String?"];
        let optional = render(&parse(&words(&optional)).unwrap(), "x");
        assert_eq!(optional.matches("serde_json::json!({").count(), 2);
        let scratch = ScratchDir::new("test-steps");
        let steps = instructions(scratch.path(), &resource());
        assert!(steps.contains("siderite-testkit"), "{steps}");
        let manifest = "[dev-dependencies]\nsiderite-testkit = \"1\"\n";
        std::fs::write(scratch.path().join("Cargo.toml"), manifest).unwrap();
        let steps = instructions(scratch.path(), &resource());
        assert!(!steps.contains("siderite-testkit"), "{steps}");
        assert!(steps.contains("siderite test"), "{steps}");
    }
}
