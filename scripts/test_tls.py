"""Run verified database transports against owned disposable TLS services."""

import json
import os
import secrets
import signal
import socket
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def command(arguments):
    """Run a fixture command without echoing passwords or private keys.

    Args:
        arguments: Argument sequence supplied directly to the process.

    Returns:
        Captured stdout; raises a sanitized failure on nonzero status.
    """
    result = subprocess.run(arguments, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"fixture command failed: {arguments[0]}")
    return result.stdout.strip()


def certificates(folder):
    """Create fixture roots and a server certificate for localhost only.

    Args:
        folder: Private temporary directory for ephemeral fixture keys.

    Returns:
        No value; certificate files are written to the temporary directory.
    """
    for stem in ("ca", "wrong-ca"):
        command([
            "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
            "-keyout", str(folder / f"{stem}.key"),
            "-out", str(folder / f"{stem}.pem"), "-days", "2",
            "-subj", f"/CN=Siderite fixture {stem}",
            "-addext", "basicConstraints=critical,CA:TRUE",
        ])
    command([
        "openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes",
        "-keyout", str(folder / "server.key"),
        "-out", str(folder / "server.csr"), "-subj", "/CN=localhost",
    ])
    extension = folder / "server.ext"
    extension.write_text(
        "subjectAltName=DNS:localhost\n"
        "basicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature,keyEncipherment\n"
        "extendedKeyUsage=serverAuth\n"
    )
    command([
        "openssl", "x509", "-req", "-in", str(folder / "server.csr"),
        "-CA", str(folder / "ca.pem"), "-CAkey", str(folder / "ca.key"),
        "-CAcreateserial", "-out", str(folder / "server.pem"),
        "-days", "2", "-extfile", str(extension),
    ])


def start(name, image, port, folder, user, arguments, environment=None):
    """Start one named service with private keys copied to its own filesystem.

    Args:
        name: Unique owned container name.
        image: Service image.
        port: Internal TCP port, published on loopback with a random host port.
        folder: Temporary certificate directory mounted read-only.
        user: Service account owning private key copies.
        arguments: Arguments for the image entrypoint after key preparation.
        environment: Optional container environment dictionary.

    Returns:
        Published port after the TCP listener becomes available.
    """
    preparation = (
        "set -e; "
        "cp /fixture/server.pem /fixture/server.key /fixture/ca.pem /tmp/; "
        "cat /tmp/server.pem /tmp/server.key > /tmp/server-combined.pem; "
        f"chown {user}:{user} /tmp/server.pem /tmp/server.key /tmp/ca.pem; "
        f"chown {user}:{user} /tmp/server-combined.pem; "
        "chmod 600 /tmp/server.key /tmp/server-combined.pem; "
        'exec docker-entrypoint.sh "$@"'
    )
    run = [
        "docker", "run", "-d", "--name", name,
        "--label", "siderite.fixture=tls",
        "-p", f"127.0.0.1::{port}",
        "-v", f"{folder}:/fixture:ro", "--entrypoint", "sh",
    ]
    for key, value in (environment or {}).items():
        run.extend(["-e", f"{key}={value}"])
    command([*run, image, "-c", preparation, "fixture", *arguments])
    mapping = command(["docker", "port", name, f"{port}/tcp"])
    host_port = int(mapping.splitlines()[0].rsplit(":", 1)[1])
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", host_port), 0.2):
                return host_port
        except OSError:
            time.sleep(0.2)
    raise RuntimeError(f"fixture listener did not start: {name}")


def main():
    """Own fixture services through validation and unconditional removal.

    Returns:
        Cargo test status; setup errors fail with sanitized diagnostics.
    """
    def terminate(_signal, _frame):
        raise SystemExit("fixture run interrupted")

    signal.signal(signal.SIGTERM, terminate)
    names = []
    fixture_id = uuid.uuid4().hex[:12]
    password = secrets.token_hex(24)
    try:
        fixture_root = ROOT / "target"
        fixture_root.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="siderite-tls-", dir=fixture_root,
        ) as location:
            folder = Path(location)
            certificates(folder)
            name = f"siderite-tls-pg-{fixture_id}"
            names.append(name)
            pg = start(name, "postgres:17-alpine", 5432, folder, "postgres", [
                "postgres", "-c", "ssl=on",
                "-c", "ssl_cert_file=/tmp/server.pem",
                "-c", "ssl_key_file=/tmp/server.key",
            ], {"POSTGRES_PASSWORD": password})
            name = f"siderite-tls-mysql-{fixture_id}"
            names.append(name)
            mysql = start(name, "mysql:8.4", 3306, folder, "mysql", [
                "mysqld", "--ssl-ca=/tmp/ca.pem",
                "--ssl-cert=/tmp/server.pem", "--ssl-key=/tmp/server.key",
                "--require-secure-transport=ON",
            ], {"MYSQL_ROOT_PASSWORD": password})
            name = f"siderite-tls-redis-{fixture_id}"
            names.append(name)
            redis = start(name, "redis:7-alpine", 6379, folder, "redis", [
                "redis-server", "--port", "0", "--tls-port", "6379",
                "--tls-cert-file", "/tmp/server.pem",
                "--tls-key-file", "/tmp/server.key",
                "--tls-ca-cert-file", "/tmp/ca.pem",
                "--tls-auth-clients", "no", "--save", "",
            ])
            name = f"siderite-tls-mongo-{fixture_id}"
            names.append(name)
            mongo = start(name, "mongo:8", 27017, folder, "mongodb", [
                "mongod", "--bind_ip_all", "--tlsMode", "requireTLS",
                "--tlsCertificateKeyFile", "/tmp/server-combined.pem",
                "--tlsCAFile", "/tmp/ca.pem",
                "--tlsAllowConnectionsWithoutCertificates",
            ], {"GLIBC_TUNABLES": "glibc.pthread.rseq=1"})
            env = os.environ.copy()
            env.update({
                "SIDERITE_TLS_CA": str(folder / "ca.pem"),
                "SIDERITE_TLS_WRONG_CA": str(folder / "wrong-ca.pem"),
                "SIDERITE_TLS_PG": (
                    f"postgres://postgres:{password}@localhost:{pg}/postgres"
                    f"?sslmode=verify-full&sslrootcert={folder}/ca.pem"
                ),
                "SIDERITE_TLS_MYSQL": (
                    f"mysql://root:{password}@localhost:{mysql}/mysql"
                    f"?ssl-mode=VERIFY_IDENTITY&ssl-ca={folder}/ca.pem"
                ),
                "SIDERITE_TLS_NATIVE": (
                    f"mysql://root:{password}@localhost:{mysql}/mysql"
                ),
                "SIDERITE_TLS_REDIS": f"rediss://localhost:{redis}/15",
                "SIDERITE_TLS_MONGO": (
                    f"mongodb://localhost:{mongo}/?tls=true"
                    f"&tlsCAFile={folder}/ca.pem"
                    "&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000"
                ),
            })
            print("Disposable TLS services ready; verifying transports.",
                  flush=True)
            result = subprocess.run([
                "cargo", "test", "-p", "siderite-backends", "--all-features",
                "--test", "tls_contract", "--", "--ignored",
            ], cwd=ROOT, env=env)
            evidence = {
                "verified": result.returncode == 0,
                "services": ["postgres:17-alpine", "mysql:8.4",
                             "redis:7-alpine", "mongo:8"],
                "contracts": ["valid trusted TLS", "wrong hostname",
                              "wrong trust root", "native and SQLx MySQL"],
                "fixture_scope": "owned disposable containers only",
            }
            output = ROOT / "target" / "tls-contract.json"
            output.write_text(json.dumps(evidence, indent=2) + "\n")
            return result.returncode
    finally:
        for name in reversed(names):
            subprocess.run(["docker", "rm", "-f", name],
                           stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, check=False)
        print("Owned TLS fixture services removed.", flush=True)


if __name__ == "__main__":
    raise SystemExit(main())
