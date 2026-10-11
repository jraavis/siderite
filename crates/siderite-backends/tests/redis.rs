//! Redis end-to-end tests.
//!
//! These tests are explicitly ignored in offline runs. Select --ignored
//! with the documented service URL to run them; missing configuration fails.
//! Each test owns a disposable schema/database or a unique Redis namespace.
//! Shared application data and Redis FLUSH commands are not used.
#![cfg(feature = "redis")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::time::Duration;

use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use siderite_backends::redis::{RedisError, RedisStore, Ttl};
use siderite_orm::{BackendError, OrmError, QueryError};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Widget {
    id: u32,
    name: String,
}

async fn with_namespace<F, Fut>(test: F)
where
    F: FnOnce(RedisStore) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = std::env::var("REDIS_URL")
        .ok()
        .filter(|value| value.starts_with("redis"))
    else {
        panic!("live Redis tests require REDIS_URL");
    };
    let prefix = format!("siderite:{}:", uuid::Uuid::new_v4().simple());
    let store = RedisStore::connect(&url).await.unwrap().with_prefix(prefix);
    assert_eq!(
        store.database(),
        15,
        "REDIS_URL must select database 15 (selected {})",
        store.database()
    );
    let cleanup = store.clone();
    let result = AssertUnwindSafe(test(store)).catch_unwind().await;
    let cleaned = cleanup.delete_namespace().await;
    if let Err(err) = &cleaned {
        eprintln!("redis prefix cleanup failed: {err}");
    }
    if let Err(panic) = result {
        resume_unwind(panic);
    }
    cleaned.unwrap();
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn string_keys_ttl_and_counters() {
    with_namespace(|store| async move {
        assert_eq!(store.get("missing").await.unwrap(), None);
        assert_eq!(store.ttl("missing").await.unwrap(), Ttl::Missing);
        assert!(!store.exists("missing").await.unwrap());

        store.set("name", "ada", None).await.unwrap();
        assert_eq!(store.get("name").await.unwrap().as_deref(), Some("ada"));
        assert!(store.exists("name").await.unwrap());
        assert_eq!(store.ttl("name").await.unwrap(), Ttl::Persistent);
        let clone = store.clone();
        assert_eq!(clone.get("name").await.unwrap().as_deref(), Some("ada"));

        store
            .set("name", "", Some(Duration::from_secs(5)))
            .await
            .unwrap();
        assert_eq!(store.get("name").await.unwrap().as_deref(), Some(""));
        match store.ttl("name").await.unwrap() {
            Ttl::ExpiresIn(left) => {
                assert!(left > Duration::ZERO && left <= Duration::from_secs(5));
            }
            other => panic!("expected a ttl, got {other:?}"),
        }
        // A later SET without a TTL clears the expiry.
        store.set("name", "ada", None).await.unwrap();
        assert_eq!(store.ttl("name").await.unwrap(), Ttl::Persistent);

        assert!(!store.set_nx("name", "other").await.unwrap());
        assert_eq!(store.get("name").await.unwrap().as_deref(), Some("ada"));
        assert_eq!(store.del("name").await.unwrap(), 1);
        assert_eq!(store.del("name").await.unwrap(), 0);
        assert!(store.set_nx("name", "fresh").await.unwrap());
        assert_eq!(store.get("name").await.unwrap().as_deref(), Some("fresh"));

        // `set_nx_px` creates with a TTL in one command, truncating to ms.
        assert!(
            store
                .set_nx_px("nx-px", "a", Duration::from_micros(1_900_500))
                .await
                .unwrap()
        );
        match store.ttl("nx-px").await.unwrap() {
            Ttl::ExpiresIn(left) => {
                assert!(left > Duration::ZERO && left <= Duration::from_millis(1900));
            }
            other => panic!("expected a ttl, got {other:?}"),
        }
        // A loser keeps the value and deadline; invalid TTLs send nothing.
        assert!(
            !store
                .set_nx_px("nx-px", "b", Duration::from_secs(60))
                .await
                .unwrap()
        );
        assert_eq!(store.get("nx-px").await.unwrap().as_deref(), Some("a"));
        for bad in [Duration::ZERO, Duration::from_micros(999), Duration::MAX] {
            assert!(matches!(
                store.set_nx_px("nx-bad", "x", bad).await,
                Err(RedisError::Invalid(_))
            ));
        }
        assert_eq!(store.get("nx-bad").await.unwrap(), None);

        assert!(
            !store
                .expire("absent", Duration::from_secs(5))
                .await
                .unwrap()
        );
        assert!(store.expire("name", Duration::from_secs(30)).await.unwrap());
        match store.ttl("name").await.unwrap() {
            Ttl::ExpiresIn(left) => {
                assert!(left > Duration::ZERO && left <= Duration::from_secs(30));
            }
            other => panic!("expected a ttl, got {other:?}"),
        }

        assert_eq!(store.incr_by("hits", 2).await.unwrap(), 2);
        assert_eq!(store.incr_by("hits", 3).await.unwrap(), 5);
        assert_eq!(store.incr_by("hits", -1).await.unwrap(), 4);
        assert_eq!(store.get("hits").await.unwrap().as_deref(), Some("4"));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn prefixes_do_not_collide() {
    with_namespace(|store| async move {
        let left = store.with_prefix(format!("{}left:", store.prefix()));
        let right = store.with_prefix(format!("{}right:", store.prefix()));
        assert_eq!(left.key("user"), format!("{}left:user", store.prefix()));
        assert_eq!(right.prefix(), format!("{}right:", store.prefix()));
        left.set("user", "from-left", None).await.unwrap();
        right.set("user", "from-right", None).await.unwrap();
        assert_eq!(
            left.get("user").await.unwrap().as_deref(),
            Some("from-left")
        );
        assert_eq!(
            right.get("user").await.unwrap().as_deref(),
            Some("from-right")
        );
        // The parent prefix plus the remainder is the same physical key.
        assert_eq!(
            store.get("left:user").await.unwrap().as_deref(),
            Some("from-left")
        );
        assert_eq!(store.get("user").await.unwrap(), None);
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn hashes() {
    with_namespace(|store| async move {
        assert_eq!(store.hget("profile", "name").await.unwrap(), None);
        assert!(store.hgetall("profile").await.unwrap().is_empty());
        assert_eq!(store.hset("profile", "name", "ada").await.unwrap(), 1);
        assert_eq!(store.hset("profile", "name", "grace").await.unwrap(), 0);
        assert_eq!(
            store
                .hset_many("profile", [("city", "london"), ("lang", "en")])
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            store.hget("profile", "name").await.unwrap().as_deref(),
            Some("grace")
        );
        let all = store.hgetall("profile").await.unwrap();
        let expected = HashMap::from([
            ("name".to_owned(), "grace".to_owned()),
            ("city".to_owned(), "london".to_owned()),
            ("lang".to_owned(), "en".to_owned()),
        ]);
        assert_eq!(all, expected);
        assert_eq!(store.hdel("profile", ["city", "missing"]).await.unwrap(), 1);
        assert_eq!(store.hget("profile", "city").await.unwrap(), None);
        assert_eq!(store.hdel("profile", ["city"]).await.unwrap(), 0);
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn sets() {
    with_namespace(|store| async move {
        assert!(store.smembers("tags").await.unwrap().is_empty());
        assert!(!store.sismember("tags", "rust").await.unwrap());
        assert_eq!(store.sadd("tags", ["rust", "redis"]).await.unwrap(), 2);
        assert_eq!(store.sadd("tags", ["rust"]).await.unwrap(), 0);
        assert!(store.sismember("tags", "redis").await.unwrap());
        assert_eq!(
            store.smembers("tags").await.unwrap(),
            HashSet::from(["rust".to_owned(), "redis".to_owned()])
        );
        assert_eq!(store.srem("tags", ["rust", "absent"]).await.unwrap(), 1);
        assert!(!store.sismember("tags", "rust").await.unwrap());
        assert_eq!(store.srem("tags", ["rust"]).await.unwrap(), 0);
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn json_round_trip() {
    with_namespace(|store| async move {
        let widget = Widget {
            id: 7,
            name: "ada".into(),
        };
        store
            .set_json("widget", &widget, Some(Duration::from_secs(30)))
            .await
            .unwrap();
        let loaded = store.get_json::<Widget>("widget").await.unwrap();
        assert_eq!(loaded, Some(widget));
        assert!(matches!(
            store.ttl("widget").await.unwrap(),
            Ttl::ExpiresIn(_)
        ));
        assert_eq!(store.get_json::<Widget>("missing").await.unwrap(), None);

        store.set("bad", "{", None).await.unwrap();
        let err = store.get_json::<Widget>("bad").await.unwrap_err();
        assert!(matches!(
            OrmError::from(err),
            OrmError::Query(QueryError::Decode { .. })
        ));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn atomic_pipeline_applies_commands_in_order() {
    with_namespace(|store| async move {
        let (first, second): (i64, i64) = store
            .pipeline(|pipe| {
                pipe.cmd("INCRBY").arg(store.key("n")).arg(1);
                pipe.cmd("INCRBY").arg(store.key("n")).arg(10);
            })
            .await
            .unwrap();
        assert_eq!((first, second), (1, 11));
        assert_eq!(store.get("n").await.unwrap().as_deref(), Some("11"));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn a_rejected_queued_command_does_not_apply_the_batch() {
    with_namespace(|store| async move {
        store.set("a", "old", None).await.unwrap();
        let err = store
            .pipeline::<String, _>(|pipe| {
                pipe.cmd("SET").arg(store.key("a")).arg("new");
                pipe.cmd("GET");
            })
            .await
            .unwrap_err();
        assert!(matches!(err, RedisError::Command(_)), "{err}");
        assert_eq!(store.get("a").await.unwrap().as_deref(), Some("old"));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn wrong_type_is_a_backend_error() {
    with_namespace(|store| async move {
        store.set("s", "hello", None).await.unwrap();
        let err = store.hget("s", "field").await.unwrap_err();
        assert!(matches!(
            OrmError::from(err),
            OrmError::Backend(BackendError::Database(_))
        ));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn invalid_arguments_do_not_create_keys() {
    with_namespace(|store| async move {
        let ttl = store.set("k", "v", Some(Duration::ZERO)).await.unwrap_err();
        assert!(matches!(ttl, RedisError::Invalid(_)));
        assert!(matches!(
            OrmError::from(ttl),
            OrmError::Query(QueryError::InvalidPlan(_))
        ));
        assert!(
            store
                .set("k", "v", Some(Duration::from_micros(500)))
                .await
                .is_err()
        );
        assert!(
            store
                .hset_many("k", Vec::<(&str, &str)>::new())
                .await
                .is_err()
        );
        assert!(store.hdel("k", Vec::<&str>::new()).await.is_err());
        assert!(store.sadd("k", Vec::<&str>::new()).await.is_err());
        assert!(store.srem("k", Vec::<&str>::new()).await.is_err());
        assert!(!store.exists("k").await.unwrap());
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn delete_namespace_leaves_other_prefixes() {
    with_namespace(|store| async move {
        let outside = store.with_prefix(format!("other:{}", store.prefix()));
        store.set("mine", "1", None).await.unwrap();
        outside.set("mine", "2", None).await.unwrap();
        let removed = store.delete_namespace().await.unwrap();
        let mine = store.get("mine").await.unwrap();
        let theirs = outside.get("mine").await.unwrap();
        outside.delete_namespace().await.unwrap();
        assert!(removed >= 1);
        assert_eq!(mine, None);
        assert_eq!(theirs.as_deref(), Some("2"));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn empty_prefix_delete_is_refused() {
    with_namespace(|store| async move {
        store.set("keep", "1", None).await.unwrap();
        let bare = store.with_prefix("");
        let err = bare.delete_namespace().await.unwrap_err();
        assert!(matches!(err, RedisError::Invalid(_)));
        assert_eq!(store.get("keep").await.unwrap().as_deref(), Some("1"));
        assert!(store.exists("keep").await.unwrap());
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live service"]
async fn marker_scripts_admit_count_and_rotate() {
    use siderite_backends::redis::{MarkerAdmission, MarkerReply};
    with_namespace(|store| async move {
        let window = Duration::from_secs(600);
        store.set("epoch", "e1", Some(window)).await.unwrap();
        let call = |marker, token, epoch| MarkerAdmission {
            marker,
            token,
            epoch_key: "epoch",
            epoch,
            budget_prefix: "budget:",
            max_new: 2,
            window,
        };
        let stale = store.admit_marker(call("m", "t", "other")).await;
        assert_eq!(stale.unwrap(), MarkerReply::Stale);
        for (marker, token) in [("a", "ta"), ("b", "tb")] {
            let reply = store.admit_marker(call(marker, token, "e1")).await;
            assert_eq!(reply.unwrap(), MarkerReply::Admitted(token.into()));
        }
        // A live marker is returned without spending budget.
        let again = store.admit_marker(call("a", "zz", "e1")).await;
        assert_eq!(again.unwrap(), MarkerReply::Admitted("ta".into()));
        let over = store.admit_marker(call("c", "tc", "e1")).await.unwrap();
        let MarkerReply::Exhausted(left) = over else {
            panic!("expected exhaustion, got {over:?}");
        };
        assert!(!left.is_zero() && left <= window);
        assert_eq!(store.get("c").await.unwrap(), None);
        // One atomic rotation: epoch replaced with a TTL, marker deleted.
        store
            .rotate_marker("a", "epoch", "e2", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(store.get("a").await.unwrap(), None);
        assert_eq!(store.get("epoch").await.unwrap().as_deref(), Some("e2"));
        let too_long = Duration::from_millis(u64::MAX / 2);
        let zero = store.rotate_marker("a", "epoch", "e3", Duration::ZERO);
        assert!(matches!(zero.await, Err(RedisError::Invalid(_))));
        let huge = MarkerAdmission {
            window: too_long,
            ..call("d", "td", "e2")
        };
        assert!(matches!(
            store.admit_marker(huge).await,
            Err(RedisError::Invalid(_))
        ));
    })
    .await;
}
