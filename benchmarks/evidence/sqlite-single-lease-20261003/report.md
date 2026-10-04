
BENCHMARK RESULTS SUMMARY

### Database-backed Todo API

| Test | Median req/s (S / F) | Paired ratio (95% CI) | p99 ms (S / F) | Adoption / Strong lead |
|---|---|---|---|---|
| `sqlite-wal-full-fastapi-pooled-sync: insert` | 14,322 / 8,551 | 1.678x (1.630-1.745) | 2 / 3 | pass / pass |

Adoption: paired point >=1.10x, lower bound >parity. Strong lead: lower bound >1.10x. Both require >=5 identified pairs, every timed trial >=10 s, and each paired p99 ratio <=1.05.
Intervals describe the measured host and workload; they do not establish a lead across all deployments.

