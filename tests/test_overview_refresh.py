import json
import tempfile
import threading
import unittest
from datetime import datetime, timedelta
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch
from urllib.request import urlopen

from tests.test_usage_store import record
from tests.test_usage_store_api import QuietHandler
from tokenusage import pricing, web_server
from tokenusage.usage_store import UsageStore


class OverviewRefreshTests(unittest.TestCase):
    def test_http_refresh_reuses_totals_while_board_clock_stays_exact(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            store = UsageStore(root / "usage.sqlite", root / "backups")
            store.initialize(root / "missing.json")
            now = int(datetime(2026, 2, 20, 12).timestamp())
            edge = datetime(2026, 2, 20) - timedelta(days=364)
            one, two = record(record_id="a", timestamp=now - 3600), record(record_id="b", timestamp=int(edge.timestamp()) + 1)
            store.ingest({"records": [one, two], "source_status": {"codex": "ready"}}, ["codex"])
            traces = []
            connect = store.connect
            def traced():
                conn = connect()
                conn.set_trace_callback(traces.append)
                return conn
            store.connect = traced
            with patch.object(web_server, "_usage_store", store), patch.object(pricing, "PATH", root / "prices.json"):
                httpd = ThreadingHTTPServer(("127.0.0.1", 0), QuietHandler)
                thread = threading.Thread(target=httpd.serve_forever, daemon=True)
                thread.start()
                def query(timestamp, filter=""):
                    with urlopen(f"http://127.0.0.1:{httpd.server_port}/api/usage?now={timestamp}{filter}", timeout=5) as response:
                        return json.load(response)
                try:
                    first = query(now, "&include_board=0")
                    query(now + 1, "&include_board=0&source=claude")
                    second = query(now + 2, "&include_board=0")
                    self.assertEqual(first, second)
                    self.assertEqual(sum("COUNT(*) AS count, MIN(r.timestamp)" in sql for sql in traces), 2,
                                     "second unfiltered refresh should reuse its aggregates")
                    self.assertEqual(first["board_totals"], [])
                    self.assertFalse(any("SUM(CASE WHEN r.timestamp>=" in sql for sql in traces))
                    exact = query(now)
                    changed = query(now + 1)
                    self.assertEqual(exact["board_totals"][0], one["total_tokens"])
                    self.assertEqual(changed["board_totals"][0], 0)
                    self.assertEqual(sum("COUNT(*) AS count, MIN(r.timestamp)" in sql for sql in traces), 2)
                    midnight = query(int(datetime(2026, 2, 21).timestamp()), "&include_board=0")
                    self.assertEqual(len(first["calendar_records"]), 2)
                    self.assertEqual(len(midnight["calendar_records"]), 1)
                    store.ingest({"records": [record(record_id="c", timestamp=now)], "source_status": {"codex": "ready"}}, ["codex"])
                    self.assertEqual(query(now + 3, "&include_board=0")["record_count"], 3)
                finally:
                    httpd.shutdown()
                    httpd.server_close()
                    thread.join(2)


if __name__ == "__main__":
    unittest.main()
