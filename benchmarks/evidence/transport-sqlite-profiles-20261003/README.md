# Transport and SQLite profile smoke acceptance

Date: 2026-10-03. Source and release binary hashes are in metadata.json.
The release build includes owned HTTP/HTTP2 shutdown and mysql-native.
The SQLite file was temporary and removed after the run. Each app ran in
isolation with schema-preserving resets, one pair per durability profile,
500 timed inserts at concurrency 10 and pool capacity 10.

All six trials passed preflight, complete committed-row delta and independent
returned-ID/title/done response samples: 3,000 timed committed rows and 60
sampled inserts across default, WAL/FULL and WAL/NORMAL profiles. No load
generator errors were accepted. Raw trial output and per-profile paired
records are retained here. Effective application PRAGMA query readback is
not yet included; these checks exercise the configured profiles.

These are short correctness smoke runs. None meets the ten-second or
five-pair publication minimum; no confidence interval or performance lead
is established. The paired records explicitly report the failed lead gate.
WAL/NORMAL changes the acknowledgement durability policy and must not be
combined with default or WAL/FULL results.
