"""Explicit SQLite lifetime and baseline strategy for the benchmark app."""

import queue
import sqlite3
from contextlib import contextmanager

MODES = ("pooled", "pooled-sync", "per-request")


class SQLiteConnections:
    """Use native synchronous SQLite with a named connection strategy.

    ``pooled`` keeps ``pool_size`` connections, borrowed from worker threads
    so the same number of connections serve requests concurrently as in the
    Siderite app. ``pooled-sync`` keeps one connection confined to the event
    loop; its handlers contain no await while borrowing it, and blocking
    SQLite delays unrelated loop work. ``per-request`` opens and closes one.
    """

    def __init__(self, path, profile, mode, pool_size=1):
        """Validate the baseline without opening a file.

        Args:
            path: Owned benchmark database file.
            profile: default, wal-full or wal-normal durability.
            mode: pooled, pooled-sync or per-request connection strategy.
            pool_size: Connections kept by ``pooled``; others use one.

        Returns:
            None; open pooled resources during application lifespan.
        """
        if profile not in ("default", "wal-full", "wal-normal"):
            raise ValueError("Invalid SQLite durability profile")
        if mode not in MODES:
            raise ValueError("Invalid SQLite connection strategy")
        if pool_size < 1:
            raise ValueError("SQLite pool size must be positive")
        self.path, self.profile, self.mode = path, profile, mode
        self.pool_size = pool_size if mode == "pooled" else 1
        self._shared = None
        self._idle = None
        self._owned = []

    def _connect(self, threads=False):
        # One statement commits on acknowledgement, matching SQLx autocommit.
        # The default five second busy timeout matches SQLx.
        connection = sqlite3.connect(
            self.path, isolation_level=None, check_same_thread=not threads
        )
        try:
            connection.execute("PRAGMA foreign_keys = ON")
            sync = "NORMAL" if self.profile == "wal-normal" else "FULL"
            connection.execute(f"PRAGMA synchronous = {sync}")
        except BaseException:
            connection.close()
            raise
        return connection

    def open(self):
        """Open the retained connections on the application thread.

        Returns:
            None after opening; duplicate opens are rejected.
        """
        if self._shared is not None or self._idle is not None:
            raise RuntimeError("SQLite benchmark connection already open")
        if self.mode == "pooled-sync":
            self._shared = self._connect()
        elif self.mode == "pooled":
            idle = queue.LifoQueue()
            try:
                for _ in range(self.pool_size):
                    connection = self._connect(threads=True)
                    self._owned.append(connection)
                    idle.put(connection)
            except BaseException:
                self.close()
                raise
            self._idle = idle

    @contextmanager
    def borrow(self):
        """Borrow a connection, waiting for an idle one in ``pooled`` mode.

        Yields:
            A connection in autocommit mode.
        """
        if self.mode == "per-request":
            connection = self._connect()
            try:
                yield connection
            finally:
                connection.close()
            return
        if self.mode == "pooled":
            if self._idle is None:
                raise RuntimeError("SQLite benchmark lifespan has not started")
            connection = self._idle.get()
            try:
                yield connection
            finally:
                self._idle.put(connection)
            return
        if self._shared is None:
            raise RuntimeError("SQLite benchmark lifespan has not started")
        yield self._shared

    def close(self):
        """Close every retained connection.

        Returns:
            None after closing; repeated cleanup is harmless.
        """
        if self._shared is not None:
            self._shared.close()
            self._shared = None
        for connection in self._owned:
            connection.close()
        self._owned = []
        self._idle = None
