//! Shared live-service fixtures for the MongoDB tests.

use crate::common::{Seed, seed};
use chrono::{DateTime, TimeZone, Utc};
use siderite_backends::mongodb::{Keys, MongoBackend};
use siderite_orm::{BackendCapabilityError, Db, Feature, OrmError};
use uuid::Uuid;

pub struct TestDb {
    pub db: Db,
    pub backend: MongoBackend,
}

impl TestDb {
    pub async fn open() -> Option<Self> {
        let Some(url) = std::env::var("MONGODB_URL")
            .ok()
            .filter(|u| u.starts_with("mongodb"))
        else {
            panic!("live mongodb tests require MONGODB_URL");
        };
        let name = format!("siderite_test_{}", Uuid::new_v4().simple());
        let backend = MongoBackend::connect(&url, &name)
            .await
            .unwrap()
            .with_keys(Keys::default().with("tags", "slug"));
        Some(Self {
            db: Db::new(backend.clone()),
            backend,
        })
    }

    pub async fn seeded() -> Option<(Self, Seed)> {
        let t = Self::open().await?;
        let seed = seed(&t.db).await;
        Some((t, seed))
    }

    pub async fn cleanup(self) {
        self.backend.database().drop().await.unwrap();
    }
}

pub fn is_capability(result: &Result<impl std::fmt::Debug, OrmError>, wanted: Feature) -> bool {
    matches!(
        result,
        Err(OrmError::Capability(BackendCapabilityError::Unsupported { feature, .. }))
            if *feature == wanted
    ) || matches!(
        (result, wanted),
        (
            Err(OrmError::Capability(
                BackendCapabilityError::RowLockingUnsupported { .. }
            )),
            Feature::RowLocking
        )
    )
}

pub fn ms(t: DateTime<Utc>) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(t.timestamp_millis()).unwrap()
}
