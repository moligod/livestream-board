from __future__ import annotations

import json
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from tokenusage import pricing, web_server
from tokenusage.usage_store import UsageStore
from tests.test_usage_store import record


class QuietHandler(web_server.Handler):
    def log_message(self, *_args):
        pass


class UsageStoreApiTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.store = UsageStore(root / "usage.sqlite", root / "backups")
        self.store.initialize(root / "missing.json")
        self.patchers = [patch.object(web_server, "_usage_store", self.store),
                         patch.object(web_server, "SETTINGS_PATH", root / "settings.json"),
                         patch.object(pricing, "PATH", root / "prices.json")]
        for item in self.patchers:
            item.start()

    def tearDown(self):
        for item in reversed(self.patchers):
            item.stop()
        self.temp.cleanup()

    def test_failed_collector_forces_full_replay_before_returning_to_deltas(self):
        self.store.ingest({"records": [record(record_id="old")], "source_status": {"codex": "ready"}}, ["codex"])
        with patch.object(web_server, "_source_fingerprint", return_value="new"), patch.object(web_server.subprocess, "run", side_effect=RuntimeError("interrupted")):
            with self.assertRaisesRegex(RuntimeError, "interrupted"):
                web_server._collect_fresh_store(["codex"], True, False)
        self.assertEqual(self.store.status()["pending_sources"], ["codex"])
        response = {"records": [record(record_id="old"), record(session="new", record_id="new")], "source_status": {"codex": "ready"}}
        result = type("Result", (), {"returncode": 0, "stdout": json.dumps(response), "stderr": ""})()
        with patch.object(web_server, "_source_fingerprint", return_value="new"), patch.object(web_server.subprocess, "run", return_value=result) as run:
            web_server._collect_fresh_store(["codex"], True, False)
        self.assertNotIn("--changes-only", run.call_args.args[0])
        self.assertEqual(self.store.status()["record_count"], 2)
        self.assertEqual(self.store.status()["pending_sources"], [])
        empty = type("Result", (), {"returncode": 0, "stdout": '{"records":[],"source_status":{"codex":"ready"}}', "stderr": ""})()
        with patch.object(web_server, "_source_fingerprint", return_value="new"), patch.object(web_server.subprocess, "run", return_value=empty) as run:
            web_server._collect_fresh_store(["codex"], True, False)
        self.assertIn("--changes-only", run.call_args.args[0])
        self.assertEqual(self.store.status()["record_count"], 2)

    def test_http_summary_returns_only_the_requested_page(self):
        items = [record(session=str(i), timestamp=1000 + i, record_id=str(i)) for i in range(45)]
        self.store.ingest({"records": items, "source_status": {"codex": "ready"}}, ["codex"])
        server = ThreadingHTTPServer(("127.0.0.1", 0), QuietHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            url = f"http://127.0.0.1:{server.server_address[1]}/api/usage?now=1200&page=2&page_size=20"
            with urlopen(url, timeout=5) as response:
                result = json.load(response)
            self.assertEqual(result["record_count"], 45)
            self.assertEqual(len(result["records"]), 20)
            self.assertEqual(result["page"], 2)
            self.assertEqual(result["records"][0]["timestamp"], 1024)
            self.assertEqual(result["latest_record_id"], 45)
            self.assertEqual([row["row_id"] for row in result["records"]], list(range(25, 5, -1)))
            with self.assertRaises(HTTPError) as caught:
                urlopen(Request(url, headers={"Origin": "https://example.com"}), timeout=5)
            self.assertEqual(caught.exception.code, 403)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(2)

    def test_restoring_older_backup_forces_full_replay_if_collector_cache_advanced(self):
        original = record(record_id="old")
        self.store.ingest({"records": [original], "source_status": {"codex": "ready"}}, ["codex"], fingerprints={"codex": "before"}, cache_stamps={"codex": "cache-before"})
        backup = self.store.backup_if_due()
        newer = record(session="new", timestamp=1100, record_id="new")
        self.store.ingest({"records": [newer], "source_status": {"codex": "ready"}}, ["codex"], fingerprints={"codex": "after"}, cache_stamps={"codex": "cache-after"})
        restored = UsageStore(backup, Path(self.temp.name) / "restore-backups")
        response = {"records": [original, newer], "source_status": {"codex": "ready"}}
        result = type("Result", (), {"returncode": 0, "stdout": json.dumps(response), "stderr": ""})()
        with patch.object(web_server, "_usage_store", restored), patch.object(web_server, "_source_fingerprint", return_value="after"), patch.object(web_server, "_collector_cache_stamp", return_value="cache-after"), patch.object(web_server.subprocess, "run", return_value=result) as run:
            metadata, _ = web_server._collect_fresh_store(["codex"], False, False)
        self.assertNotIn("--changes-only", run.call_args.args[0])
        self.assertEqual(metadata["record_count"], 2)
        self.assertEqual(metadata["source_fingerprints"]["codex"], "after")


if __name__ == "__main__":
    unittest.main()
