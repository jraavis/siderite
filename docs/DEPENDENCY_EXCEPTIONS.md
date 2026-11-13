# Dependency exposure decisions

## RUSTSEC-2023-0071: rsa

Reviewed: 2026-10-03. Locked packages: rsa 0.9.10, sqlx-mysql 0.8.6.
Responsible role: backend/release maintainers; no named assignee is recorded.
Review on every relevant lockfile/callpath change and before a release.

[RustSec](https://rustsec.org/advisories/RUSTSEC-2023-0071.html) still lists
no patched versions. The advisory concerns timing disclosure from private-key
operations. An advisory exception is not a claim that the dependency is safe
for arbitrary cryptographic use.

The inspected framework and benchmark Rust sources have no `RsaPrivateKey`
use or direct rsa signing/decryption call. SQLx MySQL authentication imports
`RsaPublicKey` and performs OAEP public-key encryption of the password in
`connection/auth.rs::encrypt_rsa` for SHA256/caching-SHA2 authentication over
an unencrypted connection. The server supplies that public key. The TLS path
sends the authentication password inside the encrypted transport and skips
this RSA exchange. No affected client private-key operation was found in
this pinned driver callpath; this does not audit the database server.

Decision: retain the narrow cargo-deny exception while this is the only
reviewed rsa exposure. Use certificate- and hostname-verified TLS for remote
databases. Public-key password encryption without verified transport does
not authenticate the database peer. Application dependencies adding RSA
private-key operations need their own review; this rationale does not cover
such use, another driver version or an unverified dependency graph.

Exit condition: remove the exception when the locked driver stops depending
on affected rsa, or adopts a patched release. Any new private-key callpath
invalidates this rationale. Record the new review before release instead of
broadening the ignore list. See [TLS configuration](BACKEND_TLS.md).

## RUSTSEC-2026-0459: libsqlite3-sys (bundled SQLite)

Reviewed: 2026-11-13. Locked packages: libsqlite3-sys 0.30.1, sqlx-sqlite
0.8.6. Responsible role: backend/release maintainers; no named assignee is
recorded. Review on every relevant lockfile/callpath change and before a
release.

[RustSec](https://rustsec.org/advisories/RUSTSEC-2026-0459.html): SQLite
before 3.54.0 can return uninitialized page-cache memory when a process
opens a database file together with a `-wal` file whose page size differs.
The fix is libsqlite3-sys 0.39 (SQLite 3.54). No SQLx release can use it:
sqlx-sqlite 0.8.6 and 0.9.0 both require `libsqlite3-sys >=0.30.1, <0.38`,
so neither a lockfile update nor the SQLx 0.9 upgrade removes it.

Exposure: the attack needs the victim process to open a database file and
its `-wal` that a less trusted party wrote. Siderite opens SQLite only at
URLs the application developer configures (settings, `DATABASE_URL`,
`--database`); `siderite doctor --live` opens the project's own database
read-only. No callpath opens database files supplied by end users or other
untrusted parties.

Decision: retain the narrow cargo-deny exception. Applications must not
open SQLite files (or their `-wal`) from untrusted sources while it
stands. This rationale does not cover application code that does.

Exit condition: remove the exception when a SQLx release allows
libsqlite3-sys 0.39 or later, and update to it. A callpath that opens
untrusted database files invalidates this rationale.
