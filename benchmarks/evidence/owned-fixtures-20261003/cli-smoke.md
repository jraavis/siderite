
BENCHMARK RESULTS SUMMARY

### Database-backed Todo API

| Test | Median req/s (S / F) | Paired ratio (95% CI) | p99 ms (S / F) | Lead gate |
|---|---|---|---|---|
| `sqlite-wal-full: insert` | 3,675 / 3,386 | 1.085x (insufficient pairs) | 74 / 5 | not met |

Lead gate: >=5 identified pairs, every timed trial >=10 s, 95% paired lower bound strictly >1.10x.
Intervals describe the measured host and workload; they do not establish a lead across all deployments.

