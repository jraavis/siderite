//! Readiness checks: outcomes, timeouts, caching, single flight and the
//! concurrency cap.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

type Counter = Arc<AtomicUsize>;
use std::sync::Arc;

fn counting(runs: &Counter) -> impl Fn() -> CheckFuture + Send + Sync + use<> {
    let runs = Arc::clone(runs);
    move || {
        let runs = Arc::clone(&runs);
        Box::pin(async move {
            runs.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(())
        })
    }
}

#[tokio::test]
async fn outcomes_are_ok_failed_or_timeout() {
    let checks = ReadinessChecks::new()
        .timeout(Duration::from_millis(50))
        .check("fast", || async { Ok(()) })
        .check("broken", || async { Err("secret detail".to_owned()) })
        .check("slow", || async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(())
        });
    let report = checks.report(true).await;
    assert_eq!(report.status, "unavailable");
    assert_eq!(report.checks["fast"], CheckStatus::Ok);
    assert_eq!(report.checks["broken"], CheckStatus::Failed);
    assert_eq!(report.checks["slow"], CheckStatus::Timeout);
    let public = serde_json::to_string(&report).unwrap_or_default();
    assert!(!public.contains("secret"), "{public}");
    let all_ok = ReadinessChecks::new().check("fast", || async { Ok(()) });
    assert_eq!(all_ok.report(true).await.status, "ready");
    assert_eq!(ReadinessChecks::new().report(true).await.status, "ready");
}

#[tokio::test]
async fn no_check_runs_while_the_server_is_not_ready() {
    let runs = Counter::default();
    let checks = ReadinessChecks::new().check("db", counting(&runs));
    let report = checks.report(false).await;
    assert_eq!(report.status, "unavailable");
    assert!(report.checks.is_empty());
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn evaluations_are_cached_and_shared() {
    let runs = Counter::default();
    let checks = ReadinessChecks::new()
        .cache(Duration::from_secs(60))
        .check("db", counting(&runs));
    let probes = (0..20).map(|_| {
        let checks = checks.clone();
        tokio::spawn(async move { checks.report(true).await })
    });
    for probe in futures_util::future::join_all(probes).await {
        assert_eq!(probe.map(|r| r.status).ok(), Some("ready"));
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1, "one evaluation for all");
    let uncached = ReadinessChecks::new()
        .cache(Duration::ZERO)
        .check("db", counting(&runs));
    uncached.report(true).await;
    uncached.report(true).await;
    assert_eq!(runs.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn at_most_the_concurrency_limit_runs_at_once() {
    let active = Counter::default();
    let peak = Counter::default();
    let mut checks = ReadinessChecks::new().concurrency(2);
    for n in 0..6 {
        let (active, peak) = (Arc::clone(&active), Arc::clone(&peak));
        checks = checks.check(format!("c{n}"), move || {
            let (active, peak) = (Arc::clone(&active), Arc::clone(&peak));
            async move {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }
        });
    }
    let report = checks.report(true).await;
    assert_eq!(report.checks.len(), 6);
    assert_eq!(peak.load(Ordering::SeqCst), 2);
}
