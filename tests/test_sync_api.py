from __future__ import annotations

import json
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from unittest.mock import patch

from tokenusage import pricing, web_server


class QuietHandler(web_server.Handler):
    def log_message(self, *_args):
        pass


class SyncApiTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.patchers = [patch.object(pricing, "PATH", root / "prices.json"),
                         patch.object(web_server, "SYNC_SETTINGS_PATH", root / "sync.json"),
                         patch.object(web_server, "SETTINGS_PATH", root / "sources.json")]
        for patcher in self.patchers:
            patcher.start()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), QuietHandler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(2)
        for patcher in reversed(self.patchers):
            patcher.stop()
        self.temp.cleanup()

    def request(self, path: str, method: str = "GET", body=None, origin: str | None = None):
        headers = {"Content-Type": "application/json"}
        if origin:
            headers["Origin"] = origin
        data = None if body is None else json.dumps(body).encode()
        with urlopen(Request(self.base + path, data=data, method=method, headers=headers), timeout=3) as response:
            return json.load(response)

    def test_settings_and_manual_pricing_are_persisted_via_http(self) -> None:
        config = self.request("/api/sync-settings", "PUT", {"auto_scan_enabled": False, "interval_seconds": 5})
        self.assertEqual(config, self.request("/api/sync-settings"))
        self.assertFalse(config["auto_scan_enabled"])
        model = {"model_id": "test-model", "name": "Test", "provider": "vendor", "input": 1, "output": 2, "cache_read": 0.1, "cache_write": None}
        saved = self.request("/api/pricing/entry", "PUT", model)
        self.assertEqual(saved["prices"][0]["origin"], "manual")
        self.assertEqual(self.request("/api/pricing")["prices"][0]["model_id"], "test-model")

    def test_models_dev_selection_and_sync_through_http(self) -> None:
        model = {"key": "vendor/test", "model_id": "test", "name": "Test", "provider": "vendor", "input": 1, "output": 2, "cache_read": 0.1, "cache_write": None, "origin": "models.dev"}
        with patch.object(pricing, "catalog", return_value=[model]):
            self.assertEqual(len(self.request("/api/pricing/catalog")["models"]), 1)
            self.request("/api/pricing/settings", "PUT", {"selected": [model["key"]]})
            result = self.request("/api/pricing/sync", "POST", {})
        self.assertEqual(result["updated"], 1)
        self.assertEqual(result["state"]["prices"][0]["model_id"], "test")

    def test_large_select_all_is_saved_in_full(self) -> None:
        selected = [f"provider/model-{index:05d}-with-long-name" for index in range(6000)]
        self.assertGreater(len(json.dumps({"selected": selected}).encode()), 64 * 1024)
        result = self.request("/api/pricing/settings", "PUT", {"selected": selected})
        self.assertEqual(result["selected"], selected)
        self.assertEqual(self.request("/api/pricing")["selected"], selected)

    def test_concurrent_sync_is_rejected_without_queuing(self) -> None:
        with web_server._scan_request_lock:
            with self.assertRaises(HTTPError) as caught:
                self.request("/api/scan", "POST", {"sources": ["codex"], "force": False})
        self.assertEqual(caught.exception.code, 409)

    def test_foreign_origin_cannot_modify_sync_or_pricing(self) -> None:
        for path, body in [("/api/sync-settings", {"auto_scan_enabled": False}), ("/api/pricing/settings", {"selected": []})]:
            with self.assertRaises(HTTPError) as caught:
                self.request(path, "PUT", body, "https://example.com")
            self.assertEqual(caught.exception.code, 403)


if __name__ == "__main__":
    unittest.main()
