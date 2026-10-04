#!/usr/bin/env python3
"""Automated benchmark runner for Siderite vs FastAPI.

Matches the benchmark methodology documented in docs/BENCHMARKS.md.
"""

from __future__ import annotations

import argparse
import importlib
import json
import math
import os
import signal
import subprocess
import sys
import time
import tempfile
from contextlib import nullcontext
from dataclasses import asdict
from pathlib import Path
from typing import Any, Dict, Optional, Tuple

from latency_csv import apply_percentiles
from benchmark_metadata import write_metadata
from benchmark_metrics import (
    BenchRunResult,
    BenchmarkError,
    MetricSummary,
    parse_ab,
    require_valid,
    retain_trial,
    summarize_runs,
)
from database_checks import inspect_database, preflight_insert
from response_checks import check_insert_responses
from paired_comparison import compare_pairs
from owned_fixture import owned_fixture
from server_process import ServerSpec, running_server
from database_fixtures import (
    SQLITE_DB,
    reset_sqlite_file,
    resolve_db_url,
    seed_database,
)

ROOT_DIR = Path(__file__).resolve().parent.parent
BENCHMARKS_DIR = ROOT_DIR / "benchmarks"
FASTAPI_DIR = BENCHMARKS_DIR / "fastapi"
LOGS_DIR = BENCHMARKS_DIR / "logs"
LOGS_DIR.mkdir(parents=True, exist_ok=True)
TRIALS_DIR = LOGS_DIR / time.strftime("trials-%Y%m%d-%H%M%S")


def check_prerequisites():
    """Verify ab and python dependencies are available."""
    if (
        not os.path.exists("/usr/sbin/ab")
        and not subprocess.run(["which", "ab"], capture_output=True).returncode
        == 0
    ):
        print(
            "ERROR: 'ab' (ApacheBench) is not found in PATH or /usr/sbin/ab.",
            file=sys.stderr,
        )
        sys.exit(1)

    try:
        for dependency in ("fastapi", "pydantic", "uvicorn"):
            importlib.import_module(dependency)
    except ImportError as e:
        print(f"ERROR: Missing Python dependency: {e}", file=sys.stderr)
        print(
            "Run: pip install -r benchmarks/fastapi/requirements.txt",
            file=sys.stderr,
        )
        sys.exit(1)


def is_port_in_use(port: int) -> bool:
    import socket

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        return s.connect_ex(("127.0.0.1", port)) == 0


def run_ab(
    url: str,
    n: int,
    c: int,
    method: str = "GET",
    post_data: Optional[str] = None,
    content_type: str = "application/json",
    keep_alive: bool = False,
    timeout: float = 120.0,
) -> BenchRunResult:
    if n < 100 or c < 1 or c > n or not math.isfinite(timeout) or timeout <= 0:
        raise ValueError("Require n >=100, n >=c >=1 and a finite timeout >0")
    TRIALS_DIR.mkdir(parents=True, exist_ok=True)
    handle, filename = tempfile.mkstemp(dir=TRIALS_DIR, prefix="latency-",
                                       suffix=".csv")
    os.close(handle)
    csv_path = Path(filename)
    cmd = ["ab", "-l", "-e", str(csv_path)]
    if keep_alive:
        cmd.append("-k")
    cmd.extend(["-n", str(n), "-c", str(c)])

    temp_post_file = None
    if method == "POST" and post_data is not None:
        temp_post_file = tempfile.NamedTemporaryFile(mode="w", delete=False)
        temp_post_file.write(post_data)
        temp_post_file.flush()
        temp_post_file.close()
        cmd.extend(["-p", temp_post_file.name, "-T", content_type])

    cmd.append(url)

    try:
        try:
            proc = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=False,
                timeout=timeout,
            )
            output = proc.stdout + proc.stderr
            result = parse_ab(
                output, proc.returncode, n, c, keep_alive=keep_alive
            )
        except subprocess.TimeoutExpired as error:

            def decoded(value):
                if isinstance(value, bytes):
                    return value.decode("utf-8", errors="replace")
                return value or ""

            output = decoded(error.stdout) + decoded(error.stderr)
            result = parse_ab(
                output, None, n, c, timed_out=True, keep_alive=keep_alive
            )
        result.latency_csv = csv_path.name
        if result.valid:
            try:
                apply_percentiles(result, csv_path.read_text())
            except (BenchmarkError, OSError) as error:
                result.invalid_reasons.append(
                    f"Latency CSV validation failed: {type(error).__name__}"
                )
        retain_trial(result, TRIALS_DIR, "load-generator")
        return result
    finally:
        if temp_post_file and os.path.exists(temp_post_file.name):
            os.remove(temp_post_file.name)


# ---------------------------------------------------------------------------
# Benchmark Runners
# ---------------------------------------------------------------------------


def run_benchmark_set(
    name: str,
    siderite_url_base: str,
    fastapi_url_base: str,
    path: str,
    n: int,
    c: int,
    method: str = "GET",
    post_data: Optional[str] = None,
    num_runs: int = 5,
    keep_alive: bool = False,
    reseed_callback=None,
    row_count_callback=None,
    preflight_callback=None,
    response_check_callback=None,
    server_context=None,
    min_seconds: float = 0.0,
    timeout: float = 120.0,
    warmup_requests: Optional[int] = None,
    database_backend: Optional[str] = None,
) -> Tuple[MetricSummary, MetricSummary]:
    print("\n=======================================================")
    print(f"Benchmark: {name} ({method} {path})")
    print(f"Parameters: -n {n} -c {c}, repetitions={num_runs}")
    print("=======================================================")

    sides = {
        "Siderite": (siderite_url_base, []),
        "FastAPI": (fastapi_url_base, []),
    }
    if min_seconds > 0:
        rates = []
        for label, (base, _) in sides.items():
            scope = server_context(label) if server_context else nullcontext()
            with scope:
                if reseed_callback:
                    reseed_callback()
                print(f"  -> Calibrating {label}...")
                pilot = run_ab(
                    f"{base}{path}",
                    max(c, 500),
                    c,
                    method,
                    post_data,
                    keep_alive=keep_alive,
                    timeout=timeout,
                )
                require_valid(pilot)
                pilot_n = max(c, math.ceil(pilot.req_per_sec))
                pilot = run_ab(
                    f"{base}{path}",
                    pilot_n,
                    c,
                    method,
                    post_data,
                    keep_alive=keep_alive,
                    timeout=timeout,
                )
                require_valid(pilot)
                rates.append(pilot.req_per_sec)
        n = max(n, math.ceil(max(rates) * min_seconds * 1.5))
        print(f"  -> Calibrated request count: {n} (minimum {min_seconds}s)")

    for pair in range(num_runs):
        order = list(sides) if pair % 2 == 0 else list(reversed(sides))
        for label in order:
            base, runs = sides[label]
            scope = server_context(label) if server_context else nullcontext()
            with scope as runtime:
                if reseed_callback:
                    reseed_callback()
                if preflight_callback:
                    preflight_callback(base)
                print(f"  -> Warming up {label}...")
                default_warm = min(max(100, n // 10), 500)
                warm_n = max(100, c, warmup_requests or default_warm)
                warm = run_ab(
                    f"{base}{path}",
                    warm_n,
                    c,
                    method,
                    post_data,
                    keep_alive=keep_alive,
                    timeout=timeout,
                )
                require_valid(warm)
                if reseed_callback:
                    reseed_callback()
                before = row_count_callback() if row_count_callback else None
                if before is not None and before != 100:
                    raise BenchmarkError(f"Fixture baseline count: {before}")
                print(
                    f"  -> {label} Run {pair + 1}/{num_runs}...",
                    end="",
                    flush=True,
                )
                result = run_ab(
                    f"{base}{path}",
                    n,
                    c,
                    method,
                    post_data,
                    keep_alive=keep_alive,
                    timeout=timeout,
                )
                if (
                    result.elapsed_seconds is not None
                    and result.elapsed_seconds < min_seconds
                ):
                    result.invalid_reasons.append(
                        f"Trial shorter than {min_seconds}s; recalibrate"
                    )
                if row_count_callback:
                    result.row_delta = row_count_callback() - before
                    if result.row_delta != n:
                        result.invalid_reasons.append(
                            f"Committed row delta {result.row_delta}, "
                            f"expected {n}"
                        )
                if response_check_callback and result.valid:
                    try:
                        result.response_checks = response_check_callback(
                            base, c
                        )
                        result.invalid_reasons.extend(
                            result.response_checks["invalid_reasons"]
                        )
                    except Exception as error:
                        result.invalid_reasons.append(
                            f"Response check failed: {type(error).__name__}"
                        )
                result.pair_index = pair
                result.runtime_workers = (
                    runtime["scheduler_workers"] if runtime else None
                )
                result.database_backend = database_backend
                result.database_settings = (
                    runtime.get("database_settings") if runtime else None
                )
                record = retain_trial(
                    result, TRIALS_DIR, f"{name}-{label}-{pair}"
                )
                if not result.valid:
                    print(f" INVALID (evidence: {record})")
                require_valid(result)
                print(f" {result.req_per_sec:,.0f} req/s")
                runs.append(result)

    siderite_runs = sides["Siderite"][1]
    fastapi_runs = sides["FastAPI"][1]
    siderite_summary = summarize_runs(siderite_runs)
    fastapi_summary = summarize_runs(fastapi_runs)
    comparison = compare_pairs(siderite_runs, fastapi_runs)
    siderite_summary.paired_comparison = comparison
    TRIALS_DIR.mkdir(parents=True, exist_ok=True)
    safe_label = "".join(character if character.isalnum() else "_"
                         for character in name)[:100]
    (TRIALS_DIR / f"{safe_label}-paired-comparison.json").write_text(
        json.dumps(comparison, indent=2) + "\n"
    )

    ratio = comparison["paired_geometric_ratio"]
    print(
        f"==> Result: Siderite = {siderite_summary.median_rps:,.0f} req/s | "
        f"FastAPI = {fastapi_summary.median_rps:,.0f} req/s | "
        f"Speedup: {ratio:.2f}x\n"
    )
    interval = comparison["percentile_interval"]
    if interval:
        print(f"Paired geometric ratio: "
              f"{comparison['paired_geometric_ratio']:.3f}x; "
              f"95% interval: {interval[0]:.3f}-{interval[1]:.3f}x; "
              f"lead gate passed: {comparison['lead_gate_passed']}\n")

    return siderite_summary, fastapi_summary


def server_specs(
    binary: str,
    app: str,
    siderite_port: int,
    fastapi_port: int,
    health_path: str,
    env=None,
    runtime_workers: int = 1,
):
    """Build process descriptions without opening application pools.

    Args:
        binary: Release binary name.
        app: Python ASGI module name.
        siderite_port: Siderite listening port.
        fastapi_port: FastAPI listening port.
        health_path: Readiness route.
        env: Optional environment for both applications.
        runtime_workers: Rust scheduler workers; Python has one process/loop.

    Returns:
        Mapping from application label to process specification.
    """
    env = dict(os.environ if env is None else env)
    env.update({"TOKIO_WORKER_THREADS": str(runtime_workers),
                "BENCHMARK_RUNTIME": "1"})
    executable = ROOT_DIR / "target" / "release" / binary
    if not executable.exists():
        raise BenchmarkError(f"Build release binary first: {executable}")
    return {
        "Siderite": ServerSpec(
            [str(executable), "run", "--addr", f"127.0.0.1:{siderite_port}"],
            ROOT_DIR,
            f"http://127.0.0.1:{siderite_port}",
            health_path,
            TRIALS_DIR / f"{binary}-siderite.log",
            env,
            runtime_workers,
        ),
        "FastAPI": ServerSpec(
            [
                sys.executable,
                "-m",
                "uvicorn",
                f"{app}:app",
                "--host",
                "127.0.0.1",
                "--port",
                str(fastapi_port),
                "--workers",
                "1",
                "--log-level",
                "warning",
                "--no-access-log",
            ],
            FASTAPI_DIR,
            f"http://127.0.0.1:{fastapi_port}",
            health_path,
            TRIALS_DIR / f"{binary}-fastapi.log",
            env,
            1,
        ),
    }


def run_plain_suite(
    siderite_port: int,
    fastapi_port: int,
    num_runs: int = 5,
    fast: bool = False,
    keep_alive: bool = False,
    min_seconds: float = 10.0,
    timeout: float = 120.0,
    concurrency: Optional[int] = None,
    requests: Optional[int] = None,
    warmup_requests: Optional[int] = None,
    runtime_workers: int = 1,
) -> Dict[str, Dict[str, Any]]:
    specs = server_specs(
        "hello_world", "plain_app", siderite_port, fastapi_port, "/",
        runtime_workers=runtime_workers
    )
    results = {}
    for path, method, body in (
        ("/", "GET", None),
        ("/hello/world", "GET", None),
        ("/echo", "POST", '{"message":"benchmark test payload"}'),
    ):
        write = method == "POST"
        n = (1000 if write else 2000) if fast else (10000 if write else 20000)
        c = (25 if write else 50) if fast else (50 if write else 100)
        n, c = requests or n, concurrency or c
        s, f = run_benchmark_set(
            f"Plain {method} {path}",
            specs["Siderite"].base_url,
            specs["FastAPI"].base_url,
            path,
            n,
            c,
            method,
            body,
            num_runs=num_runs,
            keep_alive=keep_alive,
            server_context=lambda label: running_server(specs[label]),
            min_seconds=0 if fast else min_seconds,
            timeout=timeout,
            warmup_requests=warmup_requests,
        )
        results[f"{method} {path}"] = {
            "siderite": asdict(s),
            "fastapi": asdict(f),
        }
    return results


def run_todo_suite(
    backend: str,
    siderite_port: int,
    fastapi_port: int,
    num_runs: int = 5,
    fast: bool = False,
    keep_alive: bool = False,
    sqlite_wal: bool = False,
    pool_size: int = 10,
    min_seconds: float = 10.0,
    timeout: float = 120.0,
    mysql_driver: str = "sqlx",
    todo_operation: str = "all",
    sqlite_profile: Optional[str] = None,
    db_url: Optional[str] = None,
    sqlite_path: Optional[Path] = None,
    concurrency: Optional[int] = None,
    requests: Optional[int] = None,
    warmup_requests: Optional[int] = None,
    runtime_workers: int = 1,
    fastapi_sqlite_mode: str = "pooled",
    siderite_group_commit: bool = False,
) -> Dict[str, Dict[str, Any]]:
    profile_name = sqlite_profile or (
        "wal-normal" if sqlite_wal else "default"
    )
    if profile_name not in ("default", "wal-full", "wal-normal"):
        raise ValueError("Invalid SQLite durability profile")
    if backend == "sqlite":
        sqlite_pool_size(fastapi_sqlite_mode, pool_size)
    database_file = sqlite_path or SQLITE_DB
    db_url = db_url or resolve_db_url(backend)

    def seed(recreate=False):
        seed_database(backend, recreate=recreate, url=db_url,
                      sqlite_path=database_file)

    # Schema is created once before any application connection is opened.
    if backend == "sqlite":
        reset_sqlite_file(profile_name != "default", database_file)
    seed(recreate=True)
    env = os.environ.copy()
    if backend == "mongodb" and "maxPoolSize=" not in db_url:
        separator = "&" if "?" in db_url else "?"
        db_url += f"{separator}maxPoolSize={pool_size}"
    env.update(
        {
            "DATABASE_URL": db_url,
            "DATABASE_POOL_SIZE": str(pool_size),
            "DB_BACKEND": backend,
        }
    )
    if backend == "mysql":
        env["MYSQL_URL"] = db_url
        env["SIDERITE_MYSQL_DRIVER"] = mysql_driver
    elif backend == "mongodb":
        env["MONGODB_URL"] = db_url
    elif backend == "sqlite":
        env["SQLITE_PATH"] = str(database_file)
        env["FASTAPI_SQLITE_MODE"] = fastapi_sqlite_mode
        env["SQLITE_PROFILE"] = profile_name
        env["SQLITE_WAL"] = "1" if profile_name == "wal-normal" else "0"
        env["SQLITE_GROUP_COMMIT"] = "1" if siderite_group_commit else "0"
    specs = server_specs(
        "siderite_todo",
        "todo_app",
        siderite_port,
        fastapi_port,
        "/health",
        env,
        runtime_workers,
    )
    profile = f"mysql-{mysql_driver}" if backend == "mysql" else backend
    if backend == "sqlite":
        profile = (f"sqlite-{profile_name}-pool{pool_size}"
                   f"-fastapi-{fastapi_sqlite_mode}")
        if siderite_group_commit:
            # Shared commits are a different workload from FastAPI's
            # one commit per request; never report them as the same row.
            profile += "-siderite-group-commit"
    results = {}
    for path, method, name in (
        ("/todos", "GET", "list 20"),
        ("/todos/1", "GET", "get one"),
        ("/todos", "POST", "insert"),
    ):
        if todo_operation == "insert" and method != "POST":
            continue
        if todo_operation == "reads" and method == "POST":
            continue
        write = method == "POST"
        n = (500 if write else 1000) if fast else (10000 if write else 5000)
        c = (10 if write else 25) if fast else (20 if write else 50)
        n, c = requests or n, concurrency or c
        s, f = run_benchmark_set(
            f"{profile} {method} {path}",
            specs["Siderite"].base_url,
            specs["FastAPI"].base_url,
            path,
            n,
            c,
            method,
            '{"title":"Benchmark new task"}' if write else None,
            num_runs=num_runs,
            keep_alive=keep_alive,
            reseed_callback=seed,
            row_count_callback=(
                lambda: inspect_database(backend, db_url, database_file)
            )
            if write
            else None,
            preflight_callback=(
                lambda base: preflight_insert(
                    base,
                    lambda key: inspect_database(
                        backend, db_url, database_file, key
                    ),
                )
            )
            if write
            else None,
            response_check_callback=(
                lambda base, concurrency: check_insert_responses(
                    base,
                    lambda keys: inspect_database(
                        backend, db_url, database_file, keys
                    ),
                    concurrency,
                )
            )
            if write
            else None,
            server_context=lambda label: running_server(specs[label]),
            min_seconds=0 if fast else min_seconds,
            timeout=timeout,
            warmup_requests=warmup_requests,
            database_backend=backend,
        )
        results[f"{profile}: {name}"] = {
            "siderite": asdict(s),
            "fastapi": asdict(f),
        }
    return results


def sqlite_pool_size(mode: str, pool_size: int) -> int:
    """Return the SQLite connection count both applications use.

    Only ``pooled`` keeps several FastAPI connections; the other strategies
    serve one request at a time on the event loop, so Siderite must also use
    one connection for the comparison to hold equal pool sizes.

    Args:
        mode: FastAPI SQLite connection strategy.
        pool_size: Requested pool size for both applications.

    Returns:
        The matched pool size.

    Raises:
        ValueError: The strategy cannot match the requested pool size.
    """
    if mode not in ("pooled", "pooled-sync", "per-request"):
        raise ValueError("Invalid FastAPI SQLite connection strategy")
    if pool_size < 1 or (mode != "pooled" and pool_size != 1):
        raise ValueError(
            f"FastAPI SQLite mode {mode} uses one connection; "
            "pass --pool-size 1 or use pooled"
        )
    return pool_size


def print_markdown_summary(
    plain_results: Dict[str, Any], todo_results: Dict[str, Any]
):
    """Report paired comparisons alongside each application's medians.

    Args:
        plain_results: Validated plain HTTP result summaries.
        todo_results: Validated database workload result summaries.

    Returns:
        None; Markdown tables are written to stdout.
    """
    print("\nBENCHMARK RESULTS SUMMARY\n")
    for title, results in (
        ("Plain HTTP", plain_results),
        ("Database-backed Todo API", todo_results),
    ):
        if not results:
            continue
        print(f"### {title}\n")
        print("| Test | Median req/s (S / F) | Paired ratio (95% CI) | "
              "p99 ms (S / F) | Adoption / Strong lead |")
        print("|---|---|---|---|---|")
        for test, data in results.items():
            first, second = data["siderite"], data["fastapi"]
            comparison = first.get("paired_comparison")
            if comparison:
                point = comparison["paired_geometric_ratio"]
                bounds = comparison["percentile_interval"]
                interval = (f"{bounds[0]:.3f}-{bounds[1]:.3f}"
                            if bounds else "insufficient pairs")
                ratio = f"{point:.3f}x ({interval})"
                adoption = ("pass" if comparison.get("adoption_gate_passed")
                            else "not met")
                strong = ("pass" if comparison["lead_gate_passed"]
                          else "not met")
                gate = f"{adoption} / {strong}"
            else:
                ratio, gate = "unavailable", "not evaluated"
            print(f"| `{test}` | {first['median_rps']:,.0f} / "
                  f"{second['median_rps']:,.0f} | {ratio} | "
                  f"{first['median_p99_ms']:.3f} / "
                  f"{second['median_p99_ms']:.3f} | {gate} |")
        print()
    print("Adoption: paired point >=1.10x, lower bound >parity. "
          "Strong lead: lower bound >1.10x. Both require "
          ">=5 identified pairs, every timed trial >=10 s, "
          "and each paired p99 ratio <=1.05.\n"
          "Intervals describe the measured host and workload; "
          "they do not establish a lead across all deployments.\n")


def main():
    def terminate(signum, _frame):
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, terminate)
    parser = argparse.ArgumentParser(
        description="Benchmark Siderite against FastAPI"
    )
    parser.add_argument(
        "--suite",
        choices=["plain", "db", "all"],
        default="plain",
        help="Which benchmark suite to run (plain, db, or all)",
    )
    parser.add_argument(
        "--db",
        choices=["sqlite", "postgres", "mysql", "mongodb", "all"],
        default="sqlite",
        help="Database backend for todo suite",
    )
    parser.add_argument(
        "--runs",
        type=int,
        default=5,
        help="Repetitions per benchmark (default: 5)",
    )
    parser.add_argument(
        "--mysql-driver",
        choices=["sqlx", "native"],
        default="sqlx",
        help="Siderite MySQL driver; native requires its release feature",
    )
    parser.add_argument(
        "--todo-operation",
        choices=["all", "insert", "reads"],
        default="all",
        help="Select Todo profiles without running unrelated operations",
    )
    parser.add_argument(
        "--siderite-port",
        type=int,
        default=8081,
        help="Port for Siderite server",
    )
    parser.add_argument(
        "--fastapi-port",
        type=int,
        default=8082,
        help="Port for FastAPI server",
    )
    parser.add_argument(
        "--fast",
        action="store_true",
        help="Quick run with reduced request counts",
    )
    parser.add_argument(
        "--keep-alive",
        action="store_true",
        help="Enable HTTP keep-alive (-k) in ab",
    )
    parser.add_argument(
        "--sqlite-wal",
        action="store_true",
        help=(
            "SQLite: journal_mode=WAL and synchronous=NORMAL in both apps "
            "(default: SQLite defaults)"
        ),
    )
    parser.add_argument(
        "--fastapi-sqlite-mode",
        choices=["pooled", "pooled-sync", "per-request"], default="pooled",
        help="Native SQLite baseline strategy; pooled matches --pool-size",
    )
    parser.add_argument(
        "--siderite-group-commit", action="store_true",
        help="SQLite: Siderite shares commits between concurrent inserts; "
             "reported as a separately labelled workload",
    )
    parser.add_argument(
        "--sqlite-profile",
        choices=["default", "wal-full", "wal-normal"],
        help="Matched SQLite journal/sync profile; WAL FULL keeps commit sync",
    )
    parser.add_argument(
        "--pool-size",
        type=int,
        default=10,
        help=(
            "Database connection pool size in both apps (default: 10)"
        ),
    )
    parser.add_argument(
        "--min-seconds",
        type=float,
        default=10.0,
        help="Minimum trial seconds; calibrated to faster app (default: 10)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=120.0,
        help="Load generator deadline per run in seconds (default: 120)",
    )
    parser.add_argument("--concurrency", type=int,
                        help="Override concurrent requests for every profile")
    parser.add_argument("--requests", type=int,
                        help="Initial request count; normal runs calibrate up")
    parser.add_argument("--warmup-requests", type=int,
                        help="Warm-up requests per application process")
    parser.add_argument("--runtime-workers", type=int, default=1,
                        help="Rust scheduler workers; FastAPI has one loop")
    parser.add_argument(
        "--json", action="store_true", help="Print JSON result at end"
    )
    parser.add_argument(
        "--output",
        type=str,
        default="",
        help="File to write markdown report to",
    )
    args = parser.parse_args()

    controls = (args.concurrency, args.requests, args.warmup_requests,
                args.runtime_workers)
    if any(value is not None and value < 1 for value in controls):
        parser.error("request, concurrency, warm-up and worker counts >0")
    if args.requests is not None and args.requests < 100:
        parser.error("requests must be >=100 for safe percentile capture")
    if args.warmup_requests is not None and args.warmup_requests < 100:
        parser.error("warmup-requests must be at least 100 for safe capture")
    if (args.requests is not None and args.concurrency is not None
            and args.concurrency > args.requests):
        parser.error("concurrency cannot exceed requests")
    if args.runs < 1 or args.pool_size < 1:
        parser.error("runs and pool-size must be positive")
    if args.suite != "plain" and args.db in ("sqlite", "all"):
        try:
            sqlite_pool_size(args.fastapi_sqlite_mode, args.pool_size)
        except ValueError as error:
            parser.error(str(error))
    if args.sqlite_wal and args.sqlite_profile:
        parser.error("choose sqlite-profile or the legacy sqlite-wal alias")
    if args.mysql_driver == "native" and (
        args.suite == "plain" or args.db not in ("mysql", "all")
    ):
        parser.error("native MySQL requires a DB suite containing MySQL")
    if not (0 < args.min_seconds < args.timeout < float("inf")):
        parser.error("require 0 < min-seconds < timeout, both finite")
    print(f"Raw trial evidence: {TRIALS_DIR}")
    if args.fast:
        print("SMOKE RUN: reduced workloads; exclude from published results")
    check_prerequisites()
    write_metadata(ROOT_DIR, TRIALS_DIR, vars(args))

    if is_port_in_use(args.siderite_port):
        print(
            f"ERROR: Port {args.siderite_port} is already in use.",
            file=sys.stderr,
        )
        sys.exit(1)
    if is_port_in_use(args.fastapi_port):
        print(
            f"ERROR: Port {args.fastapi_port} is already in use.",
            file=sys.stderr,
        )
        sys.exit(1)

    all_plain_results = {}
    all_todo_results = {}

    if args.suite in ("plain", "all"):
        all_plain_results = run_plain_suite(
            siderite_port=args.siderite_port,
            fastapi_port=args.fastapi_port,
            num_runs=args.runs,
            fast=args.fast,
            keep_alive=args.keep_alive,
            min_seconds=args.min_seconds,
            timeout=args.timeout,
            concurrency=args.concurrency,
            requests=args.requests,
            warmup_requests=args.warmup_requests,
            runtime_workers=args.runtime_workers,
        )

    if args.suite in ("db", "all"):
        backends = (
            ["sqlite", "postgres", "mysql", "mongodb"]
            if args.db == "all"
            else [args.db]
        )
        for b in backends:
            with owned_fixture(b, resolve_db_url(b)) as (url, path):
                res = run_todo_suite(
                    backend=b,
                    siderite_port=args.siderite_port,
                    fastapi_port=args.fastapi_port,
                    num_runs=args.runs,
                    fast=args.fast,
                    keep_alive=args.keep_alive,
                    sqlite_wal=args.sqlite_wal,
                    pool_size=args.pool_size,
                    mysql_driver=args.mysql_driver,
                    todo_operation=args.todo_operation,
                    min_seconds=args.min_seconds,
                    timeout=args.timeout,
                    sqlite_profile=args.sqlite_profile,
                    db_url=url,
                    sqlite_path=path,
                    concurrency=args.concurrency,
                    requests=args.requests,
                    warmup_requests=args.warmup_requests,
                    runtime_workers=args.runtime_workers,
                    fastapi_sqlite_mode=args.fastapi_sqlite_mode,
                    siderite_group_commit=args.siderite_group_commit,
                )
                all_todo_results.update(res)

    print_markdown_summary(all_plain_results, all_todo_results)

    if args.output:
        import io

        buf = io.StringIO()
        old_stdout = sys.stdout
        sys.stdout = buf
        print_markdown_summary(all_plain_results, all_todo_results)
        sys.stdout = old_stdout
        output_path = Path(args.output)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(buf.getvalue())
        print(f"Report written to {args.output}")

    if args.json:
        combined = {"plain": all_plain_results, "todo": all_todo_results}
        print(json.dumps(combined, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (BenchmarkError, RuntimeError) as error:
        print(f"INVALID BENCHMARK: {error}", file=sys.stderr)
        sys.exit(1)
