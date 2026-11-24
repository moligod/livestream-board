import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

from tokenusage.collectors import collect_codex, collect_opencode


class CollectorTests(unittest.TestCase):
    def test_codex_uses_last_cumulative_event(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            folder = root / "sessions" / "2026" / "08" / "19"
            folder.mkdir(parents=True)
            rows = [
                {"type": "session_meta", "payload": {"id": "s1", "cwd": "E:\\demo"}},
                {"type": "turn_context", "payload": {"model": "gpt-test"}},
                {"timestamp": "2026-08-19T00:00:00Z", "type": "event_msg", "payload": {"type": "token_count", "info": {"total_token_usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}}},
                {"timestamp": "2026-08-19T00:01:00Z", "type": "event_msg", "payload": {"type": "token_count", "info": {"total_token_usage": {"input_tokens": 20, "output_tokens": 3, "total_tokens": 23}}}},
            ]
            (folder / "x.jsonl").write_text("\n".join(json.dumps(row) for row in rows), encoding="utf-8")
            result = collect_codex(root)
            self.assertEqual(result[0].total_tokens, 23)
            self.assertEqual(result[0].project, "demo")

    def test_opencode_reads_session_totals(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "opencode.db"
            conn = sqlite3.connect(path)
            conn.executescript("""
                CREATE TABLE project(id TEXT, name TEXT, worktree TEXT);
                CREATE TABLE session(id TEXT, project_id TEXT, directory TEXT, model TEXT,
                    time_updated INTEGER, tokens_input INTEGER, tokens_output INTEGER,
                    tokens_reasoning INTEGER, tokens_cache_read INTEGER,
                    tokens_cache_write INTEGER, cost REAL);
                INSERT INTO project VALUES ('p1','demo','E:/demo');
                INSERT INTO session VALUES ('s1','p1','E:/demo','{"id":"gpt-test","providerID":"demo"}',1000,10,2,1,5,0,0.1);
            """)
            conn.commit(); conn.close()
            result = collect_opencode(path)
            self.assertEqual(result[0].total_tokens, 12)
            self.assertEqual(result[0].cache_read_tokens, 5)
            self.assertEqual(result[0].model, "gpt-test")


if __name__ == "__main__":
    unittest.main()
