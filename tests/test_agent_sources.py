"""Exercise the actual Rust collector and the web ledger without touching real sessions."""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
import unittest
from contextlib import closing
from http.server import ThreadingHTTPServer
from pathlib import Path
from urllib.request import Request, urlopen
from unittest.mock import patch

from tokenusage import pricing, web_server
from tokenusage.sources import AGENT_SOURCES
from tokenusage.usage_store import UsageStore

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("TOKENUSAGE_COLLECTOR", ROOT / "src-tauri/target/release" / ("dashboard-json.exe" if os.name == "nt" else "dashboard-json")))
STAMP = "2026-10-29T13:17:42.984Z"
TS = 1793279862984


def file(root: Path, path: str, *rows: dict) -> Path:
    target = root / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")
    return target


def fixture(home: Path) -> dict[str, Path]:
    gemini = file(home, ".gemini/tmp/project/chats/session-1.json", {
        "sessionId": "gemini-s", "startTime": STAMP, "messages": [{
            "id": "g1", "type": "gemini", "timestamp": STAMP, "model": "gemini-3-pro",
            "tokens": {"input": 100, "output": 25, "cached": 40, "thoughts": 5},
        }],
    })
    grok = file(home, ".grok/sessions/project/grok-s/updates.jsonl", {
        "timestamp": STAMP, "method": "_x.ai/session/update", "params": {"update": {
            "sessionUpdate": "turn_completed", "prompt_id": "request-1",
            "usage": {"modelUsage": {"grok-build": {"inputTokens": 100, "outputTokens": 30,
                        "cachedReadTokens": 40, "costUsdTicks": 1000000}}},
        }},
    })
    message = {"type": "message", "id": "a1", "parentId": None, "timestamp": STAMP,
               "message": {"role": "assistant", "provider": "example", "model": "pi-test", "responseId": "r1",
                           "content": [], "usage": {"input": 60, "output": 30, "cacheRead": 40,
                           "cacheWrite": 0, "totalTokens": 130}, "stopReason": "stop"}}
    pi = file(home, ".pi/agent/sessions/project/one.jsonl", {"type": "session", "version": 3, "id": "pi-s", "cwd": "/fixture", "timestamp": STAMP}, message)
    omp = file(home, ".omp/agent/sessions/project/one.jsonl", {"type": "title", "v": 1, "title": "test"},
               {"type": "session", "version": 3, "id": "omp-s", "cwd": "/fixture", "timestamp": STAMP}, message)
    mm = home / ".minimax/v2/sqlite/runtime-state.sqlite"
    mm.parent.mkdir(parents=True)
    with closing(sqlite3.connect(mm)) as db:
        db.executescript("""
          CREATE TABLE local_runtime_token_usage (
            id INTEGER PRIMARY KEY, session_id TEXT, model TEXT, ts INTEGER, input_tokens INTEGER,
            output_tokens INTEGER, reasoning_tokens INTEGER, cache_read_tokens INTEGER,
            cache_write_tokens INTEGER, cost_usd REAL);
          INSERT INTO local_runtime_token_usage VALUES (1,'mm-s','m-test',1793279862984,60,30,2,40,0,0.125);
        """)
    agy = file(home, ".gemini/antigravity-cli/brain/agy-s/.system_generated/logs/transcript_full.jsonl", {
        "step_index": 1, "type": "PLANNER_RESPONSE", "status": "DONE",
        "created_at": STAMP, "input_tokens": 60, "output_tokens": 30, "cache_read_tokens": 40,
    })
    usage = {"inputOther": 60, "output": 30, "inputCacheRead": 40, "inputCacheCreation": 0}
    kimi = file(home, ".kimi-code/sessions/wd_project/kimi-s/agents/main/wire.jsonl",
        {"type": "context.append_loop_event", "time": TS - 100, "event": {"type": "step.begin", "uuid": "k1", "turnId": "0", "step": 1}},
        {"type": "llm.request", "time": TS, "agentId": "main", "kind": "loop", "turnStep": "0.1", "provider": "openai", "model": "kimi-test"},
        {"type": "context.append_loop_event", "time": TS + 1000, "event": {"type": "step.end", "uuid": "k1", "turnId": "0", "step": 1, "usage": usage, "messageId": "k1"}},
    )
    cp = file(home, ".copilot/session-state/cp-s/events.jsonl", {
        "type": "session.usage_record", "id": "cp1", "timestamp": STAMP, "data": {"usage": {
            "model": "copilot-test", "inputTokens": 100, "outputTokens": 30, "cacheReadTokens": 40,
        }},
    })
    wb = file(home, ".workbuddy/projects/project/wb-s.jsonl", {
        "id": "wb1", "timestamp": TS, "type": "message", "role": "assistant", "status": "completed", "sessionId": "wb-s",
        "providerData": {"model": "wb-test", "rawUsage": {"prompt_tokens": 100, "completion_tokens": 30,
            "total_tokens": 130, "prompt_cache_hit_tokens": 40, "prompt_cache_miss_tokens": 60}},
    })
    dsh = file(home, ".dsh/sessions/project/dsh-s/session.v4.jsonl",
        {"type": "session", "version": 4, "id": "dsh-s", "createdAt": TS - 1000, "cwd": "/fixture"},
        {"type": "assistant/message", "seq": 2, "time": TS, "data": {"turn": 1, "step": 1,
            "message": {"id": "ds1", "source": {"kind": "model", "provider": "example", "model": "dsh-test"}},
            "usage": {"inputTokens": 60, "outputTokens": 30, "cacheReadTokens": 40, "cacheWriteTokens": 0},
        }},
    )
    # A standard Zstandard frame with one raw (uncompressed) block; no extra test dependency.
    payload = dsh.read_bytes()
    compressed = dsh.with_suffix(".jsonl.zstd")
    header = b"\x28\xb5\x2f\xfd\xa0" + len(payload).to_bytes(4, "little")
    compressed.write_bytes(header + ((len(payload) << 3) | 1).to_bytes(3, "little") + payload)
    dsh.unlink()
    dsh = compressed
    return dict(zip(AGENT_SOURCES, (gemini, grok, pi, omp, mm, agy, kimi, cp, wb, dsh)))


class QuietHandler(web_server.Handler):
    def log_message(self, *_args):
        pass


@unittest.skipUnless(BIN.is_file(), "build dashboard-json --release first")
class AgentSourceTests(unittest.TestCase):
    def test_original_paths_to_sqlite_and_http_without_copy_or_double_count(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            home = root / "native"
            sources = fixture(home)
            raw = {source: hashlib.sha256(path.read_bytes()).hexdigest() for source, path in sources.items()}
            data = root / "data"
            cache = data / "cache"
            store = UsageStore(cache / "usage.sqlite", data / "backups")
            store.initialize(cache / "missing.json")
            # Prevent any machine-wide app-specific override from escaping this fixture.
            overrides = {
                "TOKENUSAGE_AGENT_HOME": str(home), "GEMINI_CLI_HOME": str(home),
                "GROK_HOME": str(home / ".grok"), "DSH_HOME": str(home / ".dsh"),
                "KIMI_CODE_HOME": str(home / ".kimi-code"), "COPILOT_HOME": str(home / ".copilot"),
                "MINIMAX_DATA_DIR": str(home / ".minimax"), "WORKBUDDY_CONFIG_DIR": str(home / ".workbuddy"),
                "PI_CODING_AGENT_DIR": "", "PI_CODING_AGENT_SESSION_DIR": "", "PI_CONFIG_DIR": "",
                "OMP_PROFILE": "", "PI_PROFILE": "", "XDG_DATA_HOME": "", "T3CODE_HOME": "",
                "TOKENUSAGE_DATA_DIR": str(data), "TOKENUSAGE_COLLECTOR": str(BIN),
            }
            with patch.dict(os.environ, overrides), patch.object(web_server, "DATA_ROOT", data), patch.object(
                web_server, "_usage_store", store
            ), patch.object(web_server, "SETTINGS_PATH", cache / "sources.json"), patch.object(pricing, "PATH", cache / "prices.json"):
                server = ThreadingHTTPServer(("127.0.0.1", 0), QuietHandler)
                thread = threading.Thread(target=server.serve_forever, daemon=True)
                thread.start()
                try:
                    url = f"http://127.0.0.1:{server.server_address[1]}"
                    def scan(force: bool = True):
                        req = Request(url + "/api/scan", data=json.dumps({"sources": list(AGENT_SOURCES), "force": force}).encode(),
                                      headers={"Content-Type": "application/json"})
                        with urlopen(req, timeout=30) as response:
                            return json.load(response)
                    status = scan()
                    self.assertEqual(status["source_status"], dict.fromkeys(AGENT_SOURCES, "ready"), status.get("source_errors"))
                    self.assertEqual(status["record_count"], 10, status)
                    with closing(store.connect()) as sql:
                        counts = dict(sql.execute("SELECT source, count(*) FROM usage_records GROUP BY source"))
                        self.assertEqual(counts, dict.fromkeys(AGENT_SOURCES, 1))
                        tokens = {row[0]: tuple(row)[1:] for row in sql.execute(
                            "SELECT source,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,total_tokens FROM usage_records"
                        )}
                        expected = dict.fromkeys(AGENT_SOURCES, (60, 30, 40, 0, 130))
                        expected["mcode"] = (60, 32, 40, 0, 132)  # MiniMax reasoning is charged as output.
                        self.assertEqual(tokens, expected)
                        self.assertEqual(sql.execute("SELECT cost FROM usage_records WHERE source='mcode'").fetchone()[0], 0.125)
                    self.assertEqual(scan(False)["record_count"], 10)
                    self.assertEqual(scan()["record_count"], 10)
                    self.assertEqual(raw, {source: hashlib.sha256(path.read_bytes()).hexdigest() for source, path in sources.items()})
                    self.assertFalse(any(p.suffix in {".jsonl", ".zstd"} for p in data.rglob("*")))
                    for path in sources.values():
                        path.unlink()
                    with urlopen(url + "/api/usage?now=1793370000", timeout=5) as response:
                        summary = json.load(response)
                    self.assertEqual(summary["record_count"], 10)
                    self.assertEqual(scan()["record_count"], 10)
                finally:
                    server.shutdown()
                    server.server_close()
                    thread.join(2)

    def test_opencode_reads_both_generations_once(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "opencode.db"
            with closing(sqlite3.connect(path)) as db:
                db.executescript("""
                  CREATE TABLE message(id TEXT,session_id TEXT,time_created INTEGER,data TEXT);
                  CREATE TABLE session_v2(id TEXT,directory TEXT);
                  CREATE TABLE session_message(id TEXT,session_id TEXT,type TEXT,time_created INTEGER,data TEXT);
                """)
                def body(n):
                    return json.dumps({"role": "assistant", "modelID": "x", "time": {"completed": TS + 1},
                                       "tokens": {"input": n, "output": 1}})
                db.executemany("INSERT INTO message VALUES (?, 's', ?, ?)", [
                    ("v1-only", TS, body(1)), ("shared", TS, body(99)),
                ])
                db.executemany("INSERT INTO session_message VALUES (?, 's', 'assistant', ?, ?)", [
                    ("v2-only", TS, body(2)), ("shared", TS, body(3)),
                ])
                db.commit()
            result = subprocess.run([str(BIN), "--sources", "opencode", "--opencode-db", str(path)],
                                    capture_output=True, encoding="utf-8", check=True, timeout=20)
            records = json.loads(result.stdout)["records"]
            self.assertEqual({r["record_id"]: r["input_tokens"] for r in records},
                             {"opencode_message:v1-only": 1, "opencode_message:v2-only": 2, "opencode_message:shared": 3})
