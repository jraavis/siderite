# Matched-engine settings and percentile smoke

Both SQLite applications reported version 3.53.4, WAL, synchronous=2,
foreign_keys=1 and busy_timeout=5000. Rust linked the installed Python SQLite
library through the explicit dependency pkg-config build override. The run
manifest matched current source/binary build inputs at startup.

Two short timed trials verified 1,000 committed rows plus 40 sampled inserts.
Fine-resolution percentile CSV passed validation. One pair and subsecond
trials cannot establish adoption or a performance lead. Raw records are in
`trials/`. The runner completed the checks but failed saving its report to a
missing directory; that output handling is now fixed. Preserve the original
failure log, and do not count this as an end-to-end CLI success.
