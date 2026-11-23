"""Check the installed package, embedded schema and graceful HTTP startup."""

import json
import os
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from collections.abc import Mapping
from http.client import HTTPResponse
from pathlib import Path
from typing import cast


def run(
    binary: str,
    directory: str,
    environment: Mapping[str, str],
    *arguments: str,
) -> subprocess.CompletedProcess[str]:
    """Run a local CLI operation without disclosing generated credentials."""
    return subprocess.run(
        [binary, *arguments],
        cwd=directory,
        env=environment,
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )


def check(binary: str) -> None:
    """Create and reopen a database, then serve the versioned health API."""
    with tempfile.TemporaryDirectory(prefix="shprotify-runtime-") as work:
        directory = Path(work)
        database = directory / "library.db"
        environment = {
            "PATH": os.environ.get("PATH", ""),
            "HOME": work,
            "SHPROTIFY_DATABASE__PATH": str(database),
        }
        version = run(binary, work, environment, "--version").stdout
        assert version.startswith("shprotify "), version
        result = run(
            binary, work, environment, "user", "create", "--name", "listener"
        )
        assert json.loads(result.stdout)["user"]["name"] == "listener"
        _ = run(
            binary, work, environment, "user", "revoke", "--name", "listener"
        )

        with sqlite3.connect(database) as connection:
            versions = connection.execute(
                """SELECT version FROM _sqlx_migrations
                   WHERE success = 1 ORDER BY version"""
            ).fetchall()
            assert versions and versions[0] == (1,), versions
            assert connection.execute("PRAGMA integrity_check").fetchall() == [
                ("ok",)
            ]
            assert not connection.execute(
                "PRAGMA foreign_key_check"
            ).fetchall()

        # The sandbox has a private loopback network; choose an available port.
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = cast(tuple[str, int], listener.getsockname())[1]
        environment["SHPROTIFY_HTTP__BIND"] = f"127.0.0.1:{port}"
        with (directory / "server.log").open("w+") as log:
            process = subprocess.Popen(
                [binary], cwd=work, env=environment, stdout=log, stderr=log
            )
            try:
                deadline = time.monotonic() + 30
                while True:
                    try:
                        with cast(
                            HTTPResponse,
                            urllib.request.urlopen(
                                f"http://127.0.0.1:{port}/api/v1/health",
                                timeout=1,
                            ),
                        ) as response:
                            assert response.status == 200
                            assert json.load(response) == {"status": "ok"}
                        break
                    except (urllib.error.URLError, TimeoutError) as error:
                        if process.poll() is not None:
                            _ = log.seek(0)
                            raise AssertionError(log.read()) from error
                        if time.monotonic() >= deadline:
                            raise AssertionError(
                                "HTTP startup timed out"
                            ) from error
                        time.sleep(0.1)
                process.send_signal(signal.SIGTERM)
                assert process.wait(timeout=30) == 0
            finally:
                if process.poll() is None:
                    process.kill()
                    _ = process.wait()


if __name__ == "__main__":
    check(sys.argv[1])
