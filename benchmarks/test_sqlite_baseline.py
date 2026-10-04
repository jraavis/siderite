"""Verify the optimized native SQLite baseline and explicit file lifetime."""

import importlib.util
import sqlite3
import tempfile
import threading
import unittest
from pathlib import Path

path = Path(__file__).parent / "fastapi" / "sqlite_connections.py"
spec = importlib.util.spec_from_file_location("sqlite_connections", path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
SQLiteConnections = module.SQLiteConnections


class SQLiteTests(unittest.TestCase):
    """Autocommit acknowledgement, named durability and cleanup are explicit."""

    def test_pooled_mode_reuses_and_closes_its_writer(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "owned.db"
            client = SQLiteConnections(path, "default", "pooled-sync")
            with self.assertRaises(RuntimeError):
                with client.borrow():
                    pass
            client.open()
            with self.assertRaises(RuntimeError):
                client.open()
            with client.borrow() as first:
                first.execute("CREATE TABLE todos (id INTEGER)")
                first.execute("INSERT INTO todos VALUES (1)")
                self.assertFalse(first.in_transaction)
            observer = sqlite3.connect(path)
            try:
                self.assertEqual(observer.execute(
                    "SELECT count(*) FROM todos").fetchone()[0], 1)
            finally:
                observer.close()
            with client.borrow() as second:
                self.assertIs(first, second)
            client.close()
            client.close()
            with self.assertRaises(sqlite3.ProgrammingError):
                first.execute("SELECT 1")

    def test_pooled_threads_mode_keeps_its_connections(self):
        with tempfile.TemporaryDirectory() as directory:
            client = SQLiteConnections(Path(directory) / "owned.db",
                                       "default", "pooled", 3)
            with self.assertRaises(RuntimeError):
                with client.borrow():
                    pass
            client.open()
            with self.assertRaises(RuntimeError):
                client.open()
            with client.borrow() as a, client.borrow() as b, \
                    client.borrow() as c:
                self.assertEqual(len({id(a), id(b), id(c)}), 3)
            seen = set()

            def borrow():
                with client.borrow() as connection:
                    connection.execute("SELECT 1")
                    seen.add(id(connection))

            workers = [threading.Thread(target=borrow) for _ in range(6)]
            for worker in workers:
                worker.start()
            for worker in workers:
                worker.join()
            self.assertTrue(seen <= {id(a), id(b), id(c)})
            client.close()
            client.close()
            with self.assertRaises(sqlite3.ProgrammingError):
                a.execute("SELECT 1")

    def test_invalid_pool_size_is_rejected(self):
        with self.assertRaises(ValueError):
            SQLiteConnections("unused.db", "default", "pooled", 0)

    def test_private_mode_closes_on_success_and_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            client = SQLiteConnections(Path(directory) / "owned.db",
                                       "default", "per-request")
            for fail in (False, True):
                try:
                    with client.borrow() as connection:
                        connection.execute("SELECT 1")
                        if fail:
                            raise RuntimeError("request failed")
                except RuntimeError:
                    pass
                with self.assertRaises(sqlite3.ProgrammingError):
                    connection.execute("SELECT 1")

    def test_each_profile_applies_effective_sync_and_foreign_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            for profile, sync in (("default", 2), ("wal-full", 2),
                                  ("wal-normal", 1)):
                for mode in ("pooled", "pooled-sync", "per-request"):
                    client = SQLiteConnections(
                        Path(directory) / "owned.db", profile, mode)
                    client.open()
                    try:
                        with client.borrow() as connection:
                            observed = connection.execute(
                                "PRAGMA synchronous").fetchone()[0]
                            self.assertEqual(observed, sync)
                            self.assertEqual(connection.execute(
                                "PRAGMA foreign_keys").fetchone()[0], 1)
                    finally:
                        client.close()
