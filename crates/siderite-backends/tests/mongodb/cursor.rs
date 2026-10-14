//! Cursor pagination on MongoDB: the shared contract fixtures with
//! non-null ordering columns, and a capability error for nullable ones.

use crate::common::cursor::run_contract;
use crate::fixtures::TestDb;

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn cursor_pages_follow_the_contract_without_nullable_terms() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    run_contract(&t.db, false).await;
    t.cleanup().await;
}
