//! Hand-written model metadata shared by the unit tests.

use siderite_orm::{
    DbType, FieldMeta, ManyToManyMeta, ModelMeta, OnDelete, RelationKind, RelationMeta, SqlType,
};

const fn pk() -> FieldMeta {
    FieldMeta {
        primary_key: true,
        auto: true,
        ..FieldMeta::new("id", "id", SqlType::BigInt)
    }
}

const fn model(name: &'static str, table: &'static str, fields: &'static [FieldMeta]) -> ModelMeta {
    ModelMeta {
        name,
        table,
        fields,
        many_to_many: &[],
        ordering: &[],
        indexes: &[],
        constraints: &[],
        managed: true,
    }
}

const fn name_field() -> FieldMeta {
    FieldMeta {
        max_length: Some(100),
        ..FieldMeta::new("name", "name", <String as DbType>::SQL_TYPE)
    }
}

static AUTHOR: ModelMeta = model("Author", "authors", &[pk(), name_field()]);

/// `Author`: `id`, `name`.
pub(crate) fn author() -> &'static ModelMeta {
    &AUTHOR
}

static BOOK: ModelMeta = model(
    "Book",
    "books",
    &[
        pk(),
        FieldMeta {
            index: true,
            relation: Some(RelationMeta {
                kind: RelationKind::ForeignKey,
                target: author,
                on_delete: OnDelete::Cascade,
                related_name: None,
            }),
            ..FieldMeta::new("author", "author_id", SqlType::BigInt)
        },
    ],
);

/// `Book`: foreign key to [`author`].
pub(crate) fn book() -> &'static ModelMeta {
    &BOOK
}

static NO_PK: ModelMeta = model("Loose", "loose", &[name_field()]);

/// A model without a primary key.
pub(crate) fn no_pk() -> &'static ModelMeta {
    &NO_PK
}

static SAME_TABLE: ModelMeta = model("Writer", "authors", &[pk()]);

/// A second model on the `authors` table.
pub(crate) fn same_table() -> &'static ModelMeta {
    &SAME_TABLE
}

static SAME_NAME: ModelMeta = model("Author", "people", &[pk()]);

/// A second model named `Author`.
pub(crate) fn same_name() -> &'static ModelMeta {
    &SAME_NAME
}

static DUP_COLUMN: ModelMeta = model(
    "Twice",
    "twice",
    &[
        pk(),
        FieldMeta::new("a", "same", SqlType::Integer),
        FieldMeta::new("b", "same", SqlType::Integer),
    ],
);

/// Two fields on one column.
pub(crate) fn dup_column() -> &'static ModelMeta {
    &DUP_COLUMN
}

static TWO_PKS: ModelMeta = model(
    "Pair",
    "pairs",
    &[
        pk(),
        FieldMeta {
            primary_key: true,
            ..FieldMeta::new("other", "other", SqlType::Integer)
        },
    ],
);

/// A model with two primary-key fields.
pub(crate) fn two_pks() -> &'static ModelMeta {
    &TWO_PKS
}

static UNIQUE_TEXT: ModelMeta = model(
    "Slugged",
    "slugged",
    &[
        pk(),
        FieldMeta {
            unique: true,
            ..FieldMeta::new("slug", "slug", SqlType::Text)
        },
    ],
);

/// A unique `TEXT` column without `max_length` (MySQL cannot key it).
pub(crate) fn unique_text() -> &'static ModelMeta {
    &UNIQUE_TEXT
}

static TAGGED: ModelMeta = ModelMeta {
    many_to_many: &[ManyToManyMeta {
        name: "authors",
        target: author,
        through_table: "tagged_authors",
        source_column: "tagged_id",
        target_column: "author_id",
        through: None,
        related_name: None,
    }],
    ..model("Tagged", "tagged", &[pk()])
};

/// A model with an auto many-to-many relation to [`author`].
pub(crate) fn tagged() -> &'static ModelMeta {
    &TAGGED
}

static TAGGED_CLASH: ModelMeta = ModelMeta {
    many_to_many: &[ManyToManyMeta {
        name: "authors",
        target: author,
        through_table: "books",
        source_column: "clash_id",
        target_column: "author_id",
        through: None,
        related_name: None,
    }],
    ..model("Clash", "clash", &[pk()])
};

/// A many-to-many whose join table collides with `books`.
pub(crate) fn tagged_clash() -> &'static ModelMeta {
    &TAGGED_CLASH
}

static UNMANAGED: ModelMeta = ModelMeta {
    managed: false,
    ..model("View", "a_view", &[pk(), name_field()])
};

/// An unmanaged model (no table is created for it).
pub(crate) fn unmanaged() -> &'static ModelMeta {
    &UNMANAGED
}

/// A uniquely named temporary directory, removed on drop.
pub(crate) struct ScratchDir(std::path::PathBuf);

impl ScratchDir {
    /// Create `siderite-cli-<tag>-<pid>-<n>` under the system temp dir.
    pub(crate) fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!("siderite-cli-{tag}-{}-{n}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(err) = std::fs::create_dir_all(&dir) {
            panic!("cannot create {}: {err}", dir.display());
        }
        Self(dir)
    }

    /// The directory.
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run `future` to completion on a fresh multi-thread runtime.
pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    match builder.enable_all().build() {
        Ok(rt) => rt.block_on(future),
        Err(err) => panic!("cannot start a runtime: {err}"),
    }
}

/// Assert that `value` matches the published report schema.
pub(crate) fn assert_schema_valid(value: &serde_json::Value) {
    use std::sync::OnceLock;
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    let validator = VALIDATOR.get_or_init(|| {
        let text = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/schemas/cli-report-v1.json"
        ));
        let schema = serde_json::from_str(text);
        let schema = schema.unwrap_or_else(|e| panic!("schema: {e}"));
        let validator = jsonschema::validator_for(&schema);
        validator.unwrap_or_else(|e| panic!("schema: {e}"))
    });
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}\n{value}");
}
