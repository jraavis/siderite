"""Regression checks for trustworthy benchmark evidence and fixture resets."""

import contextlib
import io
import json
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import run_benchmarks as runner
import database_fixtures as fixtures
from benchmark_metrics import (
    BenchmarkError,
    parse_ab,
    retain_trial,
    summarize_runs,
)
from database_checks import inspect_database, preflight_insert


OUTPUT = """Time taken for tests:   1.000 seconds
Complete requests:      100
Failed requests:        0
Requests per second:    100.00 [#/sec] (mean)
Time per request:       100.000 [ms] (mean)
Time per request:       10.000 [ms] (mean, across all concurrent requests)
  50%     10
  90%     20
  99%     30
"""


class MetricsTests(unittest.TestCase):
    """Invalid results must never contribute to successful medians."""

    def test_keep_alive_requires_actual_negotiation_for_every_request(self):
        for kept in (None, 0, 99, 101):
            extra = "" if kept is None else f"Keep-Alive requests: {kept}\n"
            with self.subTest(kept=kept):
                run = parse_ab(OUTPUT + extra, 0, 100, 10, keep_alive=True)
                self.assertFalse(run.valid)
        run = parse_ab(
            OUTPUT + "Keep-Alive requests: 100\n", 0, 100, 10,
            keep_alive=True,
        )
        self.assertTrue(run.valid)
        self.assertEqual(run.keep_alive_requests, 100)

    def test_fresh_connection_trials_reject_unexpected_reuse(self):
        run = parse_ab(OUTPUT + "Keep-Alive requests: 100\n", 0, 100, 10)
        self.assertFalse(run.valid)

    def test_valid_output_uses_latency_not_inverse_throughput(self):
        result = parse_ab(OUTPUT, 0, 100, 10)
        self.assertTrue(result.valid)
        self.assertEqual(result.time_per_req_ms, 100)
        self.assertEqual(result.non_2xx_responses, 0)

    def test_rejects_http_errors_even_without_transport_errors(self):
        result = parse_ab(OUTPUT + "Non-2xx responses: 100\n", 0, 100, 10)
        self.assertFalse(result.valid)
        self.assertEqual(result.failed_requests, 0)
        with self.assertRaises(BenchmarkError):
            summarize_runs([result])

    def test_rejects_every_missing_required_metric(self):
        for line in OUTPUT.splitlines():
            if "across all" in line:
                continue
            with self.subTest(line=line):
                result = parse_ab(OUTPUT.replace(line + "\n", ""), 0, 100, 10)
                self.assertFalse(result.valid)

    def test_process_failure_does_not_invent_metrics(self):
        result = parse_ab("", 1, 100, 10)
        self.assertFalse(result.valid)
        self.assertIsNone(result.req_per_sec)
        self.assertIsNone(result.failed_requests)

    def test_rejects_incomplete_transport_failure_and_nonzero_exit(self):
        for output, code in (
            (OUTPUT, 1),
            (OUTPUT.replace("requests:      100", "requests:      99"), 0),
            (OUTPUT.replace("requests:        0", "requests:        1"), 0),
            (OUTPUT.replace("100.00 [#/sec]", "0.00 [#/sec]"), 0),
        ):
            with self.subTest(output=output, code=code):
                self.assertFalse(parse_ab(output, code, 100, 10).valid)

    def test_retains_invalid_trial_evidence(self):
        result = parse_ab("process failed", 1, 100, 10)
        with tempfile.TemporaryDirectory() as directory:
            path = retain_trial(result, Path(directory), "test / failure")
            data = json.loads(path.read_text())
            self.assertFalse(data["valid"])
            self.assertEqual(data["raw_output"], "process failed")
            self.assertTrue(data["invalid_reasons"])

    def test_subprocess_timeout_is_invalid_and_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(runner, "TRIALS_DIR", Path(directory)):
                with patch.object(
                    runner.subprocess,
                    "run",
                    side_effect=subprocess.TimeoutExpired(
                        "ab", 1, output=b"partial stdout", stderr=b"stderr"
                    ),
                ):
                    result = runner.run_ab("http://localhost/", 100, 10)
            self.assertTrue(result.timed_out)
            self.assertFalse(result.valid)
            self.assertEqual(result.raw_output, "partial stdoutstderr")
            self.assertEqual(len(list(Path(directory).glob("*.json"))), 1)

    def test_empty_set_rejected(self):
        with self.assertRaises(BenchmarkError):
            summarize_runs([])


class FixtureTests(unittest.TestCase):
    """Reset data without invalidating schema or prepared statements."""

    def test_sqlite_reset_preserves_schema_and_committed_count(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.db"
            with patch.object(fixtures, "SQLITE_DB", path):
                with contextlib.redirect_stdout(io.StringIO()):
                    runner.seed_database("sqlite", recreate=True)
                    with contextlib.closing(sqlite3.connect(path)) as conn:
                        version = conn.execute(
                            "PRAGMA schema_version"
                        ).fetchone()
                        conn.execute(
                            "INSERT INTO todos (title) VALUES ('extra')"
                        )
                        conn.commit()
                    runner.seed_database("sqlite")
                self.assertEqual(inspect_database("sqlite", "", path), 100)
                with contextlib.closing(sqlite3.connect(path)) as conn:
                    self.assertEqual(
                        conn.execute("PRAGMA schema_version").fetchone(),
                        version,
                    )
                    self.assertEqual(
                        conn.execute("SELECT MAX(id) FROM todos").fetchone()[
                            0
                        ],
                        100,
                    )

    def test_pair_order_alternates(self):
        calls = []

        def load(url, n, c, *args, **kwargs):
            calls.append(url)
            return parse_ab(
                OUTPUT.replace("requests:      100", f"requests:      {n}"),
                0,
                n,
                c,
            )

        with tempfile.TemporaryDirectory() as directory:
            with patch.object(runner, "TRIALS_DIR", Path(directory)):
                with patch.object(runner, "run_ab", side_effect=load):
                    with contextlib.redirect_stdout(io.StringIO()):
                        s, f = runner.run_benchmark_set(
                            "test",
                            "http://s",
                            "http://f",
                            "/",
                            100,
                            10,
                            num_runs=2,
                        )
        self.assertEqual(
            calls[1::2],
            [
                "http://s/",
                "http://f/",
                "http://f/",
                "http://s/",
            ],
        )
        self.assertEqual(len(s.runs), 2)
        self.assertEqual(len(f.runs), 2)

    def test_insert_count_mismatch_is_rejected_and_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(runner, "TRIALS_DIR", Path(directory)):
                with patch.object(
                    runner,
                    "run_ab",
                    side_effect=lambda *a, **k: parse_ab(OUTPUT, 0, 100, 10),
                ):
                    with contextlib.redirect_stdout(io.StringIO()):
                        with self.assertRaises(BenchmarkError):
                            runner.run_benchmark_set(
                                "test",
                                "http://s",
                                "http://f",
                                "/",
                                100,
                                10,
                                row_count_callback=iter([100, 199]).__next__,
                            )
            record = next(Path(directory).glob("*.json"))
            self.assertEqual(json.loads(record.read_text())["row_delta"], 99)

    def test_calibrates_shared_count_and_rejects_short_trials(self):
        calls = []

        def load(url, n, c, *args, **kwargs):
            calls.append((url, n))
            output = OUTPUT.replace(
                "requests:      100", f"requests:      {n}"
            )
            return parse_ab(output, 0, n, c)

        with tempfile.TemporaryDirectory() as directory:
            with patch.object(runner, "TRIALS_DIR", Path(directory)):
                with patch.object(runner, "run_ab", side_effect=load):
                    with contextlib.redirect_stdout(io.StringIO()):
                        with self.assertRaisesRegex(
                            BenchmarkError, "shorter than 10"
                        ):
                            runner.run_benchmark_set(
                                "test",
                                "http://s",
                                "http://f",
                                "/",
                                100,
                                10,
                                min_seconds=10,
                            )
            self.assertEqual(calls[-1][1], 1500)
            record = next(Path(directory).glob("*.json"))
            self.assertFalse(json.loads(record.read_text())["valid"])

    def test_only_one_application_scope_is_active(self):
        active = []
        finished = []

        @contextlib.contextmanager
        def scope(label):
            self.assertEqual(active, [])
            active.append(label)
            try:
                yield
            finally:
                finished.append(active.pop())

        def load(url, n, c, *args, **kwargs):
            self.assertEqual(len(active), 1)
            output = OUTPUT.replace(
                "requests:      100", f"requests:      {n}"
            )
            return parse_ab(output, 0, n, c)

        with tempfile.TemporaryDirectory() as directory:
            with patch.object(runner, "TRIALS_DIR", Path(directory)):
                with patch.object(runner, "run_ab", side_effect=load):
                    with contextlib.redirect_stdout(io.StringIO()):
                        runner.run_benchmark_set(
                            "test",
                            "http://s",
                            "http://f",
                            "/",
                            100,
                            10,
                            num_runs=2,
                            server_context=scope,
                        )
        self.assertEqual(
            finished, ["Siderite", "FastAPI", "FastAPI", "Siderite"]
        )
        self.assertEqual(active, [])

    def test_preflight_requires_exact_status_and_persisted_values(self):
        for status, row in ((200, ("Benchmark new task", False)), (201, None)):
            response = io.BytesIO(
                json.dumps(
                    {
                        "id": 101,
                        "title": "Benchmark new task",
                        "done": False,
                    }
                ).encode()
            )
            response.status = status
            with patch("urllib.request.urlopen", return_value=response):
                with self.assertRaises(BenchmarkError):
                    preflight_insert("http://s", lambda key: row)


if __name__ == "__main__":
    unittest.main()


class SQLitePoolTests(unittest.TestCase):
    """SQLite comparisons always hold equal connection counts."""

    def test_pooled_mode_matches_requested_size(self):
        self.assertEqual(runner.sqlite_pool_size("pooled", 10), 10)
        self.assertEqual(runner.sqlite_pool_size("pooled", 1), 1)

    def test_single_connection_modes_require_pool_of_one(self):
        for mode in ("pooled-sync", "per-request"):
            self.assertEqual(runner.sqlite_pool_size(mode, 1), 1)
            with self.assertRaises(ValueError):
                runner.sqlite_pool_size(mode, 10)

    def test_unknown_mode_is_rejected(self):
        with self.assertRaises(ValueError):
            runner.sqlite_pool_size("threads", 1)
