//! Hand-written `ModelMeta` copied from `siderite-backends/tests/reference_model.rs`.
#![allow(dead_code, clippy::unwrap_used)]

pub mod backend;
pub mod scratch;

use siderite_orm::{
    ConstraintMeta, DbType, FieldMeta, IndexMeta, ManyToManyMeta, ModelMeta, OnDelete,
    OrderDirection, RelationKind, RelationMeta, SqlType,
};

pub fn author_meta() -> &'static ModelMeta {
    &AUTHOR_META
}

pub fn book_meta() -> &'static ModelMeta {
    &BOOK_META
}

pub fn tag_meta() -> &'static ModelMeta {
    &TAG_META
}

pub fn post_meta() -> &'static ModelMeta {
    &POST_META
}

pub fn unmanaged_meta() -> &'static ModelMeta {
    &UNMANAGED_META
}

/// Self-referencing tree: `parent_id` points back at the model's own table.
pub fn node_meta() -> &'static ModelMeta {
    &NODE_META
}

static AUTHOR_META: ModelMeta = ModelMeta {
    name: "Author",
    table: "authors",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", <i64 as DbType>::SQL_TYPE)
        },
        FieldMeta {
            max_length: Some(100),
            ..FieldMeta::new("name", "name", <String as DbType>::SQL_TYPE)
        },
    ],
    many_to_many: &[],
    ordering: &[("name", OrderDirection::Asc)],
    indexes: &[],
    constraints: &[],
    managed: true,
};

static BOOK_META: ModelMeta = ModelMeta {
    name: "Book",
    table: "books",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", <i64 as DbType>::SQL_TYPE)
        },
        FieldMeta::new("title", "title", <String as DbType>::SQL_TYPE),
        FieldMeta {
            index: true,
            relation: Some(RelationMeta {
                kind: RelationKind::ForeignKey,
                target: author_meta,
                on_delete: OnDelete::Cascade,
                related_name: Some("books"),
            }),
            ..FieldMeta::new("author", "author_id", SqlType::BigInt)
        },
        FieldMeta {
            nullable: <Option<i32> as DbType>::NULLABLE,
            ..FieldMeta::new("pages", "pages", <Option<i32> as DbType>::SQL_TYPE)
        },
    ],
    many_to_many: &[],
    ordering: &[],
    indexes: &[],
    constraints: &[],
    managed: true,
};

static TAG_META: ModelMeta = ModelMeta {
    name: "Tag",
    table: "tags",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", <i64 as DbType>::SQL_TYPE)
        },
        FieldMeta {
            unique: true,
            max_length: Some(50),
            ..FieldMeta::new("slug", "slug", <String as DbType>::SQL_TYPE)
        },
    ],
    many_to_many: &[],
    ordering: &[],
    indexes: &[],
    constraints: &[],
    managed: true,
};

static POST_META: ModelMeta = ModelMeta {
    name: "Post",
    table: "posts",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", <i64 as DbType>::SQL_TYPE)
        },
        FieldMeta::new("title", "title", <String as DbType>::SQL_TYPE),
        FieldMeta {
            default: Some(siderite_orm::DbDefault::Now),
            ..FieldMeta::new("created_at", "created_at", SqlType::Timestamp)
        },
    ],
    many_to_many: &[ManyToManyMeta {
        name: "tags",
        target: tag_meta,
        through_table: "post_tags",
        source_column: "post_id",
        target_column: "tag_id",
        through: None,
        related_name: Some("posts"),
    }],
    ordering: &[],
    indexes: &[IndexMeta {
        name: "post_title_idx",
        columns: &["title"],
        unique: false,
    }],
    constraints: &[ConstraintMeta::Check {
        name: "title_not_empty",
        sql: "length(title) > 0",
    }],
    managed: true,
};

static UNMANAGED_META: ModelMeta = ModelMeta {
    name: "LegacyView",
    table: "legacy_view",
    fields: &[FieldMeta::new("id", "id", SqlType::BigInt)],
    many_to_many: &[],
    ordering: &[],
    indexes: &[],
    constraints: &[],
    managed: false,
};

static NODE_META: ModelMeta = ModelMeta {
    name: "Node",
    table: "nodes",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", <i64 as DbType>::SQL_TYPE)
        },
        FieldMeta {
            index: true,
            nullable: true,
            relation: Some(RelationMeta {
                kind: RelationKind::ForeignKey,
                target: node_meta,
                on_delete: OnDelete::SetNull,
                related_name: Some("children"),
            }),
            ..FieldMeta::new("parent", "parent_id", SqlType::BigInt)
        },
    ],
    many_to_many: &[],
    ordering: &[],
    indexes: &[],
    constraints: &[],
    managed: true,
};

pub fn temp_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "siderite-mig-{}-{}-{}",
        std::process::id(),
        nanos,
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
