from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from tokenusage import pricing


def entry(model: str, provider: str, input_price: float = 1.0) -> dict:
    return {"key": f"{provider}/{model}", "model_id": model, "name": model, "provider": provider,
            "input": input_price, "output": 5.0, "cache_read": 0.1, "cache_write": None, "origin": "models.dev"}


class PricingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.path_patch = patch.object(pricing, "PATH", Path(self.temp.name) / "prices.json")
        self.path_patch.start()

    def tearDown(self) -> None:
        self.path_patch.stop()
        self.temp.cleanup()

    def test_auto_sync_is_opt_in_and_requires_selected_models(self) -> None:
        with patch.object(pricing, "catalog") as catalog:
            self.assertTrue(pricing.sync(automatic=True)["skipped"])
            catalog.assert_not_called()
        with self.assertRaises(ValueError):
            pricing.update_settings({"auto_sync": True})
        self.assertFalse(pricing.state()["auto_sync"])

    def test_sync_preserves_manual_prices_and_updates_imported_prices(self) -> None:
        pricing.save_entry({"model_id": "model-a", "name": "Mine", "provider": "custom", "input": 7, "output": 8})
        pricing.update_settings({"selected": ["vendor/model-a", "vendor/model-b"], "auto_sync": True})
        with patch.object(pricing, "catalog", return_value=[entry("model-a", "vendor"), entry("model-b", "vendor", 3)]):
            result = pricing.sync()
        prices = {item["model_id"]: item for item in result["state"]["prices"]}
        self.assertEqual((result["updated"], result["preserved"]), (1, 1))
        self.assertEqual(prices["model-a"]["input"], 7)
        self.assertEqual(prices["model-b"]["input"], 3)
        self.assertTrue(pricing.state()["auto_sync"])

    def test_deleting_a_price_removes_tracking_so_it_is_not_recreated(self) -> None:
        pricing.update_settings({"selected": ["vendor/model-a"], "auto_sync": True})
        with patch.object(pricing, "catalog", return_value=[entry("model-a", "vendor")]):
            pricing.sync()
        result = pricing.delete_entry("model-a")
        self.assertEqual(result["prices"], [])
        self.assertEqual(result["selected"], [])
        self.assertFalse(result["auto_sync"])

    def test_duplicate_provider_selection_fails_without_overwriting_prices(self) -> None:
        pricing.save_entry({"model_id": "safe", "input": 1, "output": 2})
        pricing.update_settings({"selected": ["one/model-a", "two/model-a"]})
        with patch.object(pricing, "catalog", return_value=[entry("model-a", "one"), entry("model-a", "two")]):
            with self.assertRaisesRegex(ValueError, "多个供应商"):
                pricing.sync()
        self.assertEqual([p["model_id"] for p in pricing.state()["prices"]], ["safe"])
        self.assertIn("多个供应商", pricing.state()["last_error"])

    def test_invalid_or_unavailable_prices_are_not_silently_imported(self) -> None:
        with self.assertRaises(ValueError):
            pricing.save_entry({"model_id": "bad", "input": -1, "output": 1})
        pricing.update_settings({"selected": ["vendor/gone"]})
        with patch.object(pricing, "catalog", return_value=[entry("other", "vendor")]):
            with self.assertRaisesRegex(ValueError, "均未找到"):
                pricing.sync()
        self.assertEqual(pricing.state()["last_synced_at"], 0)

    def test_catalog_preserves_missing_cache_rates_and_provider_identity(self) -> None:
        document = {"one": {"models": {"model": {"name": "Example", "cost": {"input": 0, "output": 2}}}},
                    "two": {"models": {"model": {"cost": {"input": 3, "output": 4, "cache_read": 0.5}}}}}
        class Response:
            def __enter__(self): return self
            def __exit__(self, *_args): return False
            def read(self, _size): return json.dumps(document).encode()
        with patch.object(pricing, "_catalog_cache", None), patch.object(pricing, "urlopen", return_value=Response()):
            items = pricing.catalog(force=True)
        self.assertEqual([item["key"] for item in items], ["one/model", "two/model"])
        self.assertIsNone(items[0]["cache_read"])
        self.assertEqual(items[0]["input"], 0)
        self.assertEqual(items[1]["cache_read"], 0.5)


if __name__ == "__main__":
    unittest.main()
