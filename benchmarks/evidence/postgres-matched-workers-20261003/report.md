
BENCHMARK RESULTS SUMMARY

### Database-backed Todo API

| Test | Median req/s (S / F) | Paired ratio (95% CI) | p99 ms (S / F) | Adoption / Strong lead |
|---|---|---|---|---|
| `postgres: insert` | 8,343 / 6,862 | 1.231x (1.179-1.286) | 16.357 / 28.964 | pass / pass |

Adoption: paired point >=1.10x, lower bound >parity. Strong lead: lower bound >1.10x. Both require >=5 identified pairs, every timed trial >=10 s, and each paired p99 ratio <=1.05.
Intervals describe the measured host and workload; they do not establish a lead across all deployments.

