from __future__ import annotations

import json
import threading
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from tokenusage import web_server


class WebServerCacheTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.settings_patch = patch.object(web_server, "SETTINGS_PATH", Path(self.temporary.name) / "source-settings.json")
        self.settings_patch.start()
        self.sync_settings_patch = patch.object(web_server, "SYNC_SETTINGS_PATH", Path(self.temporary.name) / "sync-settings.json")
        self.sync_settings_patch.start()
        self.board_layout_patch = patch.object(web_server, "BOARD_LAYOUT_PATH", Path(self.temporary.name) / "board-layout.json")
        self.board_layout_patch.start()
        self.snapshot_patch = patch.object(web_server, "_save_snapshot")
        self.save_snapshot_mock = self.snapshot_patch.start()
        self.old = (
            web_server._cached_payload,
            web_server._cached_records,
            web_server._cached_at,
            web_server._scan_thread,
            web_server._revision,
            web_server._encoded_revision,
            web_server._encoded_dashboard,
            web_server._encoded_dashboard_gzip,
            web_server._board_layout_cache,
            web_server._board_layout_revision,
        )
        web_server._cached_payload = None
        web_server._cached_records = []
        web_server._cached_at = 0
        web_server._scan_thread = None
        web_server._revision = 0
        web_server._encoded_revision = -1
        web_server._encoded_dashboard = b""
        web_server._encoded_dashboard_gzip = b""
        web_server._board_layout_cache = None
        web_server._board_layout_revision = 0

    def tearDown(self) -> None:
        (
            web_server._cached_payload,
            web_server._cached_records,
            web_server._cached_at,
            web_server._scan_thread,
            web_server._revision,
            web_server._encoded_revision,
            web_server._encoded_dashboard,
            web_server._encoded_dashboard_gzip,
            web_server._board_layout_cache,
            web_server._board_layout_revision,
        ) = self.old
        self.settings_patch.stop()
        self.sync_settings_patch.stop()
        self.board_layout_patch.stop()
        self.snapshot_patch.stop()
        self.temporary.cleanup()

    def test_second_request_returns_snapshot_while_refresh_is_running(self) -> None:
        snapshot = {"records": [{"session_id": "saved"}]}
        release_refresh = threading.Event()

        def slow_refresh():
            release_refresh.wait(1)
            return snapshot, snapshot["records"]

        with patch.object(web_server, "_load_snapshot", return_value=(snapshot, snapshot["records"])), patch.object(web_server, "_collect_fresh", side_effect=slow_refresh):
            first, _ = web_server._collect()
            started = time.monotonic()
            second, _ = web_server._collect()
            elapsed = time.monotonic() - started
            release_refresh.set()

        self.assertIs(first, snapshot)
        self.assertIs(second, snapshot)
        self.assertLess(elapsed, 0.1, "second browser request must not wait for a background scan")

    def test_danmaku_font_size_is_clamped_to_supported_range(self) -> None:
        self.assertEqual(web_server._normalize_danmaku_font_size(9), 10)
        self.assertEqual(web_server._normalize_danmaku_font_size(16), 16)
        self.assertEqual(web_server._normalize_danmaku_font_size(99), 32)
        self.assertEqual(web_server._normalize_danmaku_font_size("invalid"), 16)

    def test_board_widget_selection_is_persisted_in_canonical_order(self) -> None:
        path = Path(self.temporary.name) / "board-settings.json"
        with patch.object(web_server, "BOARD_SETTINGS_PATH", path):
            saved = web_server._save_board_settings({"visible": ["danmaku", "hour", "unknown"]})
            loaded = web_server._load_board_settings()
        self.assertEqual(saved["visible"], ["hour", "danmaku"])
        self.assertEqual(loaded["visible"], ["hour", "danmaku"])
        self.assertGreater(loaded["revision"], 0)

    def test_board_sources_keep_order_when_hidden_and_when_updated_by_legacy_clients(self) -> None:
        path = Path(self.temporary.name) / "board-settings.json"
        with patch.object(web_server, "BOARD_SETTINGS_PATH", path):
            path.write_text('{"visible":["hour"],"revision":7}', encoding="utf-8")
            legacy = web_server._load_board_settings()
            self.assertEqual(legacy["visible"], ["hour"])
            self.assertEqual(legacy["order"], list(web_server.BOARD_WIDGET_IDS))
            ordered = web_server._save_board_settings({"visible": ["hour"], "order": ["year", "hour", "year", "unknown"]})
            self.assertEqual(ordered["order"], ["year", "hour", "day", "week", "month", "quarter", "danmaku"])
            hidden = web_server._save_board_settings({"visible": []})
            self.assertEqual(hidden["order"], ordered["order"])
            self.assertEqual(hidden["visible"], [])
            self.assertGreater(hidden["revision"], ordered["revision"])
            self.assertEqual(web_server._load_board_settings(), hidden)
            with self.assertRaises(ValueError):
                web_server._save_board_settings({"visible": [], "order": "not a list"})
            self.assertEqual(web_server._load_board_settings(), hidden)

    def test_refresh_does_not_hold_cache_lock_during_collection(self) -> None:
        started = threading.Event()
        release = threading.Event()

        def slow_run(*_args, **_kwargs):
            started.set()
            release.wait(1)
            return type("Result", (), {"returncode": 0, "stdout": '{"records": []}', "stderr": ""})()

        with patch.object(web_server, "_source_fingerprint", return_value="changed"), patch.object(web_server.subprocess, "run", side_effect=slow_run):
            worker = threading.Thread(target=web_server._collect_fresh, args=(["codex"], True))
            worker.start()
            self.assertTrue(started.wait(0.2))
            started_at = time.monotonic()
            with web_server._lock:
                pass
            elapsed = time.monotonic() - started_at
            release.set()
            worker.join(5)
            self.assertFalse(worker.is_alive(), "scan worker must finish before test cleanup")

        self.assertLess(elapsed, 0.1, "collection must run outside the shared cache lock")

    def test_scan_does_not_overwrite_a_concurrent_source_setting_change(self) -> None:
        started = threading.Event()
        release = threading.Event()
        def slow_run(*_args, **_kwargs):
            started.set()
            release.wait(2)
            return type("Result", (), {"returncode": 0, "stdout": '{"records": [], "source_status": {"codex": "ready"}}', "stderr": ""})()
        with patch.object(web_server, "_source_fingerprint", return_value="changed"), patch.object(web_server.subprocess, "run", side_effect=slow_run):
            worker = threading.Thread(target=web_server._collect_fresh, args=(["codex"], True))
            worker.start()
            self.assertTrue(started.wait(1))
            web_server._update_source_settings({"codex": {"enabled": False, "interval_seconds": 300}})
            release.set()
            worker.join(3)
        self.assertFalse(worker.is_alive())
        result = web_server._load_settings()["codex"]
        self.assertFalse(result["enabled"])
        self.assertEqual(result["interval_seconds"], 300)
        self.assertGreater(result["last_checked_at"], 0)

    def test_global_sync_settings_do_not_modify_per_source_preferences(self) -> None:
        web_server._update_source_settings({"codex": {"enabled": False, "interval_seconds": 300}})
        saved = web_server._update_sync_settings({"auto_scan_enabled": False, "interval_seconds": 5})
        self.assertEqual(saved, {"auto_scan_enabled": False, "interval_seconds": 5})
        self.assertEqual(web_server._load_sync_settings(), saved)
        source = web_server._load_settings()["codex"]
        self.assertFalse(source["enabled"])
        self.assertEqual(source["interval_seconds"], 300)
        with self.assertRaises(ValueError):
            web_server._update_sync_settings({"interval_seconds": 3})

    def test_unified_interval_slows_to_ten_minutes_when_overview_is_hidden(self) -> None:
        self.assertEqual(web_server._effective_scan_interval({"interval_seconds": 5}, True), 5)
        self.assertEqual(web_server._effective_scan_interval({"interval_seconds": 5}, False), 600)
        self.assertEqual(web_server._effective_scan_interval({"interval_seconds": 60}, False), 600)

    def test_disabled_auto_sync_does_not_scan_without_a_snapshot(self) -> None:
        web_server._update_sync_settings({"auto_scan_enabled": False})
        with patch.object(web_server, "_load_snapshot", return_value=None), patch.object(web_server, "_collect_fresh") as scan:
            payload, records = web_server._collect()
        scan.assert_not_called()
        self.assertEqual(records, [])
        self.assertEqual(payload["scanned_at"], 0)

    def test_manual_scan_populates_disabled_auto_sync_placeholder_even_if_fingerprint_matches(self) -> None:
        web_server._update_sync_settings({"auto_scan_enabled": False})
        settings = web_server._load_settings()
        settings["codex"]["fingerprint"] = "unchanged"
        web_server._save_settings(settings)
        result = type("Result", (), {"returncode": 0, "stdout": '{"records": [{"source": "codex", "session_id": "recovered"}], "source_status": {"codex": "ready"}}', "stderr": ""})()
        with patch.object(web_server, "_load_snapshot", return_value=None), patch.object(web_server, "_source_fingerprint", return_value="unchanged"), patch.object(web_server.subprocess, "run", return_value=result) as run:
            web_server._collect()
            payload, _ = web_server._collect_fresh(["codex"], force=False)
        run.assert_called_once()
        self.assertEqual(payload["records"][0]["session_id"], "recovered")

    def test_changed_source_scan_merges_only_that_source(self) -> None:
        web_server._cached_payload = {
            "records": [
                {"source": "codex", "session_id": "old"},
                {"source": "trae", "session_id": "keep"},
            ],
            "source_status": {"codex": "ready", "trae": "ready"},
            "source_paths": {"codex": "old.db", "trae": "trae.db"},
            "source_errors": {},
        }
        web_server._cached_records = web_server._cached_payload["records"]
        partial = {
            "records": [{"source": "codex", "session_id": "new"}],
            "source_status": {"codex": "ready", "trae": "skipped"},
            "source_paths": {"codex": ".codex", "trae": "trae.db"},
            "source_errors": {"codex": None},
        }
        result = type("Result", (), {"returncode": 0, "stdout": __import__("json").dumps(partial), "stderr": ""})()

        with patch.object(web_server, "_source_fingerprint", return_value="new-stamp"), patch.object(web_server.subprocess, "run", return_value=result) as run:
            payload, _ = web_server._collect_fresh(["codex"], force=False)

        self.assertEqual({record["session_id"] for record in payload["records"]}, {"new", "keep"})
        self.assertEqual(payload["source_paths"]["codex"], ".codex")
        self.assertEqual(run.call_args.args[0][-2:], ["--sources", "codex"])

    def test_unchanged_source_does_not_launch_collector(self) -> None:
        settings = web_server._load_settings()
        settings["codex"]["fingerprint"] = "same"
        web_server._save_settings(settings)
        web_server._cached_payload = {"records": [], "source_status": {}, "source_paths": {}, "source_errors": {}}
        web_server._cached_records = []

        with patch.object(web_server, "_source_fingerprint", return_value="same"), patch.object(web_server.subprocess, "run") as run:
            web_server._collect_fresh(["codex"], force=False)

        run.assert_not_called()

    def test_codex_fingerprint_follows_rollout_changes_without_state_db(self) -> None:
        root = Path(self.temporary.name) / "codex"
        folder = root / "sessions" / "2026" / "01" / "01"
        folder.mkdir(parents=True)
        session = folder / "rollout.jsonl"
        session.write_text("first\n", encoding="utf-8")
        with patch.object(web_server, "_source_paths", return_value={"codex": root}):
            first = web_server._source_fingerprint("codex")
            session.write_text("first\nsecond\n", encoding="utf-8")
            second = web_server._source_fingerprint("codex")
        self.assertNotEqual(first, second)

    def test_failed_source_scan_keeps_previous_records_and_retries(self) -> None:
        payload = {"records": [{"source": "codex", "session_id": "keep"}], "scanned_at": 1,
                   "source_status": {"codex": "ready"}, "source_paths": {"codex": ".codex"}, "source_errors": {"codex": None}}
        with web_server._lock:
            web_server._install_payload(payload, payload["records"], bump_revision=True)
        partial = {"records": [], "source_status": {"codex": "error"}, "source_paths": {"codex": ".codex"}, "source_errors": {"codex": "read failed"}}
        result = type("Result", (), {"returncode": 0, "stdout": __import__("json").dumps(partial), "stderr": ""})()
        with patch.object(web_server, "_source_fingerprint", return_value="changed"), patch.object(web_server.subprocess, "run", return_value=result):
            updated, _ = web_server._collect_fresh(["codex"], force=True)
        self.assertEqual(updated["records"][0]["session_id"], "keep")
        self.assertEqual(updated["source_status"]["codex"], "error")
        self.assertNotEqual(web_server._load_settings()["codex"]["fingerprint"], "changed")

    def test_first_scan_installs_cache_when_no_snapshot_exists(self) -> None:
        partial = {"records": [{"source": "codex", "session_id": "new"}], "scanned_at": 1,
                   "source_status": {"codex": "ready"}, "source_paths": {"codex": ".codex"}, "source_errors": {"codex": None}}
        result = type("Result", (), {"returncode": 0, "stdout": __import__("json").dumps(partial), "stderr": ""})()
        with patch.object(web_server, "_load_snapshot", return_value=None), patch.object(web_server, "_source_fingerprint", return_value="new"), patch.object(web_server.subprocess, "run", return_value=result):
            payload, records = web_server._collect_fresh(["codex"], force=True)
        self.assertIs(web_server._cached_payload, payload)
        self.assertEqual(records[0]["session_id"], "new")

    def test_codex_rebuild_passes_full_rebuild_flag(self) -> None:
        web_server._cached_payload = {"records": [], "source_status": {}, "source_paths": {}, "source_errors": {}}
        web_server._cached_records = []
        partial = {
            "records": [], "source_status": {"codex": "ready"},
            "source_paths": {"codex": str(Path.home() / ".codex")}, "source_errors": {"codex": None},
        }
        result = type("Result", (), {"returncode": 0, "stdout": __import__("json").dumps(partial), "stderr": ""})()
        with patch.object(web_server, "_source_fingerprint", return_value="rebuild"), patch.object(web_server.subprocess, "run", return_value=result) as run:
            web_server._collect_fresh(["codex"], force=True, rebuild_codex=True)

        self.assertIn("--rebuild-codex", run.call_args.args[0])

    def test_dashboard_status_is_small_and_revisioned(self) -> None:
        payload = {"records": [{"source": "codex"}], "scanned_at": 10, "source_status": {"codex": "ready"}, "source_paths": {}, "source_errors": {}}
        with web_server._lock:
            web_server._install_payload(payload, payload["records"], bump_revision=True)
        status = web_server._dashboard_status()
        self.assertEqual(status["record_count"], 1)
        self.assertGreater(status["revision"], 0)
        self.assertNotIn("records", status)

    def test_dashboard_encoding_is_cached_per_revision(self) -> None:
        payload = {"records": [{"source": "codex", "total_tokens": 1}], "scanned_at": 10, "source_status": {}, "source_paths": {}, "source_errors": {}}
        with web_server._lock:
            web_server._install_payload(payload, payload["records"], bump_revision=True)
        real_compress = web_server.gzip.compress
        with patch.object(web_server.gzip, "compress", wraps=real_compress) as compress:
            first = web_server._encoded_dashboard_response()
            second = web_server._encoded_dashboard_response()
        self.assertEqual(first, second)
        self.assertEqual(compress.call_count, 1)

    def test_concurrent_dashboard_requests_share_one_encoding(self) -> None:
        payload = {"records": [{"source": "codex", "total_tokens": 1}], "scanned_at": 10,
                   "source_status": {}, "source_paths": {}, "source_errors": {}}
        with web_server._lock:
            web_server._install_payload(payload, payload["records"], bump_revision=True)
        real_compress = web_server.gzip.compress
        started = threading.Event()
        release = threading.Event()
        def slow_compress(*args, **kwargs):
            started.set()
            release.wait(1)
            return real_compress(*args, **kwargs)
        with patch.object(web_server.gzip, "compress", side_effect=slow_compress) as compress:
            first = threading.Thread(target=web_server._encoded_dashboard_response)
            second = threading.Thread(target=web_server._encoded_dashboard_response)
            first.start()
            self.assertTrue(started.wait(1))
            second.start()
            release.set()
            first.join(2); second.join(2)
        self.assertFalse(first.is_alive() or second.is_alive())
        self.assertEqual(compress.call_count, 1)

    def test_forced_scan_with_identical_source_does_not_publish_new_revision(self) -> None:
        payload = {"records": [{"source": "opencode", "session_id": "same"}], "scanned_at": 10, "source_status": {"opencode": "ready"}, "source_paths": {"opencode": "db"}, "source_errors": {"opencode": None}}
        with web_server._lock:
            web_server._install_payload(payload, payload["records"], bump_revision=True)
        revision = web_server._revision
        partial = {**payload, "revision": None}
        result = type("Result", (), {"returncode": 0, "stdout": __import__("json").dumps(partial), "stderr": ""})()
        with patch.object(web_server, "_source_fingerprint", return_value="same-data"), patch.object(web_server.subprocess, "run", return_value=result):
            web_server._collect_fresh(["opencode"], force=True)
        self.assertEqual(web_server._revision, revision)
        self.save_snapshot_mock.assert_not_called()

    def test_board_layout_is_persisted_with_server_revision(self) -> None:
        layout = {
            "viewport": {"width": 1920, "height": 1080},
            "cards": [{"x": index * 100, "y": 20, "width": 260, "height": 174} for index in range(7)],
        }
        saved = web_server._save_board_layout(layout)
        self.assertGreater(saved["revision"], 0)
        self.assertTrue(web_server.BOARD_LAYOUT_PATH.exists())
        web_server._board_layout_cache = None
        loaded = web_server._load_board_layout()
        self.assertEqual(loaded, saved)

    def test_board_layout_rejects_missing_cards(self) -> None:
        with self.assertRaises(ValueError):
            web_server._save_board_layout({"viewport": {"width": 100, "height": 100}, "cards": []})


if __name__ == "__main__":
    unittest.main()
