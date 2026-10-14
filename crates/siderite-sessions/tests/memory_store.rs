//! `MemoryStore`: bounded capacity, expiry and revocation.
#![allow(clippy::unwrap_used)]

use siderite_sessions::{MemoryStore, SessionRecord, SessionStore};
use std::time::Duration;

const LONG: Duration = Duration::from_secs(3600);

fn record(user: Option<&str>) -> SessionRecord {
    SessionRecord {
        user: user.map(str::to_owned),
        ..SessionRecord::default()
    }
}

#[tokio::test]
async fn round_trip_and_delete() {
    let store = MemoryStore::new(4);
    assert!(store.is_empty());
    assert_eq!(store.load("a").await.unwrap(), None);
    store.save("a", &record(Some("ada")), LONG).await.unwrap();
    assert_eq!(store.load("a").await.unwrap(), Some(record(Some("ada"))));
    assert_eq!(store.len(), 1);
    store.delete("a").await.unwrap();
    store.delete("a").await.unwrap();
    assert_eq!(store.load("a").await.unwrap(), None);
}

#[tokio::test]
async fn capacity_is_bounded_and_never_evicts_live_sessions() {
    let store = MemoryStore::new(2);
    store.save("a", &record(None), LONG).await.unwrap();
    store.save("b", &record(None), LONG).await.unwrap();
    assert!(store.save("c", &record(None), LONG).await.is_err());
    assert_eq!(store.len(), 2);
    assert!(store.load("a").await.unwrap().is_some());
    // Updating an existing entry needs no room.
    store.save("a", &record(Some("ada")), LONG).await.unwrap();
    store.delete("b").await.unwrap();
    store.save("c", &record(None), LONG).await.unwrap();
    assert_eq!(store.len(), 2);
}

#[tokio::test]
async fn expired_entries_are_invisible_and_swept() {
    let store = MemoryStore::new(8);
    let short = Duration::from_millis(30);
    store.save("old", &record(None), short).await.unwrap();
    store.save("new", &record(None), LONG).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(store.load("old").await.unwrap(), None);
    assert_eq!(store.len(), 2, "expired entries stay until swept");
    assert_eq!(store.sweep(), 1);
    assert_eq!(store.len(), 1);
    assert_eq!(store.sweep(), 0);
}

#[tokio::test]
async fn a_full_store_reclaims_expired_entries_for_new_ones() {
    let store = MemoryStore::new(1);
    let short = Duration::from_millis(20);
    store.save("old", &record(None), short).await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    store.save("new", &record(None), LONG).await.unwrap();
    assert_eq!(store.len(), 1);
}

#[tokio::test]
async fn revocation_is_per_user_and_counted() {
    let store = MemoryStore::new(8);
    assert_eq!(store.auth_epoch("ada").await.unwrap(), 0);
    store.save("a", &record(Some("ada")), LONG).await.unwrap();
    store.save("b", &record(Some("bob")), LONG).await.unwrap();
    store.save("c", &record(None), LONG).await.unwrap();
    store.revoke_user("ada").await.unwrap();
    assert_eq!(store.auth_epoch("ada").await.unwrap(), 1);
    assert_eq!(store.auth_epoch("bob").await.unwrap(), 0);
    assert_eq!(store.load("a").await.unwrap(), None);
    assert!(store.load("b").await.unwrap().is_some());
    assert!(store.load("c").await.unwrap().is_some());
    // A login after the revocation carries the new epoch.
    let mut again = record(Some("ada"));
    again.auth_epoch = 1;
    store.save("d", &again, LONG).await.unwrap();
    assert!(store.load("d").await.unwrap().is_some());
}

#[tokio::test]
async fn clones_share_state() {
    let store = MemoryStore::new(2);
    let other = store.clone();
    store.save("a", &record(None), LONG).await.unwrap();
    assert!(other.load("a").await.unwrap().is_some());
}
