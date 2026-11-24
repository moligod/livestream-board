from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from tokenusage.usage_store import UsageStore


def record(source="codex", session="s", timestamp=1000, total=15, **extra):
    return {"source": source, "session_id": session, "timestamp": timestamp, "project": "demo", "model": "test-model",
            "input_tokens": total - 5, "output_tokens": 5, "reasoning_tokens": 0, "cache_read_tokens": 0,
            "cache_write_tokens": 0, "total_tokens": total, "cost": 0, **extra}


class UsageStoreTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.store = UsageStore(self.root / "usage.sqlite", self.root / "backups")

    def tearDown(self):
        self.temp.cleanup()

    def test_migration_preserves_duplicates_and_binds_existing_request_ids(self):
        first, second = record(total=15), record(total=15)
        snapshot = self.root / "dashboard.json"
        snapshot.write_text(json.dumps({"dashboard": {"records": [first, second], "scanned_at": 3}}), encoding="utf-8")
        cache = self.root / "codex-rollouts.json"
        cache.write_text(json.dumps({"entries": {"a": {"request_ids": ["one", "two"], "records": [first, second]}}}), encoding="utf-8")
        result = self.store.initialize(snapshot, cache)
        self.assertEqual(result["record_count"], 2)
        self.assertTrue(snapshot.exists())
        changed = record(total=20, record_id="two")
        metadata, did_change = self.store.ingest({"records": [record(total=15, record_id="one"), changed], "source_status": {"codex": "ready"}}, ["codex"])
        self.assertTrue(did_change)
        self.assertEqual(metadata["record_count"], 2)
        self.assertEqual(sorted(r["total_tokens"] for r in self.store.all_records()), [15, 20])
        self.assertTrue(any((self.root / "backups").glob("migration-*/dashboard.json")))

    def test_repeated_scans_and_missing_sources_keep_history(self):
        self.store.initialize(self.root / "missing.json")
        item = record(record_id="one")
        ready = {"records": [item], "source_status": {"codex": "ready"}}
        self.store.ingest(ready, ["codex"])
        self.store.ingest(ready, ["codex"])
        metadata, _ = self.store.ingest({"records": [], "source_status": {"codex": "missing"}}, ["codex"])
        self.assertEqual(metadata["record_count"], 1)
        self.assertEqual(len(list(self.store.all_records())), 1)

    def test_query_filters_paginates_and_prices_without_loading_all_rows(self):
        self.store.initialize(self.root / "missing.json")
        items = [record(session=str(i), timestamp=1000 + i, total=15, record_id=str(i)) for i in range(5)]
        self.store.ingest({"records": items, "source_status": {"codex": "ready"}}, ["codex"])
        prices = [{"model_id": "test-model", "input": 2, "output": 4, "cache_read": 1, "cache_write": 1}]
        page = self.store.query({"page": 2, "page_size": 2, "now": 1100}, prices)
        self.assertEqual(page["record_count"], 5)
        self.assertEqual([r["timestamp"] for r in page["records"]], [1002, 1001])
        self.assertEqual(page["totals"]["total"], 75)
        self.assertEqual(page["totals"]["priced"], 5)
        self.assertAlmostEqual(page["totals"]["cost"], 0.0002)
        limited = self.store.query({"start": 1002, "end": 1003, "query": "demo", "now": 1100}, prices)
        self.assertEqual(limited["record_count"], 2)
        self.assertEqual(sum(row["total_tokens"] for row in limited["trend_records"]), 30)
        self.store.ingest({"records": [record(session="new", timestamp=1005, record_id="new")], "source_status": {"codex": "ready"}}, ["codex"])
        self.assertEqual(self.store.query({"page": 2, "page_size": 2, "now": 1100}, prices)["record_count"], 6)

    def test_cost_calculation_preserves_cache_and_reported_price_semantics(self):
        self.store.initialize(self.root / "missing.json")
        trae = record(source="trae", total=110, record_id="trae", input_tokens=100, output_tokens=10, cache_read_tokens=80, cache_write_tokens=10)
        unpriced = record(source="codex", session="unpriced", record_id="no-cache-rate", cache_read_tokens=3)
        reported = record(source="codex", session="reported", record_id="reported", cost=0.125)
        self.store.ingest({"records": [trae, unpriced, reported], "source_status": {"trae": "ready", "codex": "ready"}}, ["trae", "codex"])
        prices = [{"model_id": "test-model", "input": 1, "output": 2, "cache_read": None, "cache_write": 0.2}]
        summary = self.store.query({"now": 1100}, prices)
        self.assertEqual(summary["totals"]["priced"], 1)
        self.assertEqual(summary["totals"]["unpriced"], 2)
        self.assertAlmostEqual(summary["totals"]["cost"], 0.125)
        prices[0]["cache_read"] = 0.1
        summary = self.store.query({"now": 1100}, prices)
        self.assertEqual(summary["totals"]["priced"], 3)
        expected_trae = (10 * 1 + 10 * 2 + 80 * 0.1 + 10 * 0.2) / 1_000_000
        expected_codex = (10 * 1 + 5 * 2 + 3 * 0.1) / 1_000_000
        self.assertAlmostEqual(summary["totals"]["cost"], 0.125 + expected_trae + expected_codex)

    def test_backup_is_consistent_and_is_not_recreated_before_interval(self):
        self.store.initialize(self.root / "missing.json")
        self.store.ingest({"records": [record(record_id="one")], "source_status": {"codex": "ready"}}, ["codex"])
        backup = self.store.backup_if_due()
        self.assertIsNotNone(backup)
        self.assertIsNone(self.store.backup_if_due())
        restored = UsageStore(backup, self.root / "other")
        self.assertEqual(restored.status()["record_count"], 1)
        self.assertEqual(self.store.backup_status()["count"], 1)
        self.assertGreater(self.store.backup_status()["last_backup_at"], 0)

    def test_interrupted_scan_is_marked_until_the_source_commits(self):
        self.store.initialize(self.root / "missing.json")
        self.store.mark_pending(["codex", "claude"])
        self.store.ingest({"records": [record(record_id="one")], "source_status": {"codex": "ready", "claude": "error"}}, ["codex", "claude"])
        self.assertEqual(self.store.status()["pending_sources"], ["claude"])
        self.store.ingest({"records": [], "source_status": {"claude": "missing"}}, ["claude"])
        self.assertEqual(self.store.status()["pending_sources"], [])

    def test_backup_rotation_preserves_only_recent_service_backups_and_migration_copies(self):
        self.store.initialize(self.root / "missing.json")
        self.store.backup_dir.mkdir(parents=True, exist_ok=True)
        unrelated = self.store.backup_dir / "usage-manual.sqlite"
        unrelated.write_text("manual", encoding="utf-8")
        legacy = self.store.backup_dir / "migration-keep"
        legacy.mkdir()
        for _ in range(12):
            self.assertIsNotNone(self.store.backup_if_due(0))
        self.assertEqual(len(list(self.store.backup_dir.glob("usage-2*.sqlite"))), 10)
        self.assertTrue(unrelated.exists())
        self.assertTrue(legacy.exists())

    def test_ambiguous_legacy_update_stops_instead_of_double_counting(self):
        snapshot = self.root / "dashboard.json"
        snapshot.write_text(json.dumps({"dashboard": {"records": [record()]}}), encoding="utf-8")
        self.store.initialize(snapshot)
        self.store.mark_pending(["codex"])
        with self.assertRaisesRegex(RuntimeError, "历史记录缺少可靠标识"):
            self.store.ingest({"records": [record(total=99, record_id="one")], "source_status": {"codex": "ready"}}, ["codex"])
        self.assertEqual(self.store.status()["record_count"], 1)
        self.assertEqual(self.store.status()["pending_sources"], ["codex"])

    def test_invalid_snapshot_does_not_publish_empty_database(self):
        snapshot = self.root / "dashboard.json"
        snapshot.write_text('{"dashboard":{}}', encoding="utf-8")
        with self.assertRaises(ValueError):
            self.store.initialize(snapshot)
        self.assertFalse(self.store.path.exists())
        self.assertTrue(snapshot.exists())


if __name__ == "__main__":
    unittest.main()
