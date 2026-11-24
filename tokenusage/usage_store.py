"""Durable local usage ledger and bounded dashboard queries.

The legacy JSON snapshot is imported once and left untouched.  No request
details are pruned: a source disappearing must not erase prior observations.
"""
from __future__ import annotations

import hashlib
import json
import math
import os
import re
import shutil
import sqlite3
import tempfile
import threading
import time
from collections import OrderedDict
from contextlib import closing
from pathlib import Path
from typing import Any, Iterable

from .sources import SOURCES

FIELDS = ("source", "session_id", "timestamp", "project", "model", "input_tokens", "output_tokens", "reasoning_tokens", "cache_read_tokens", "cache_write_tokens", "total_tokens", "cost")
NUMERIC_FIELDS = ("input_tokens", "output_tokens", "reasoning_tokens", "cache_read_tokens", "cache_write_tokens", "total_tokens")
SOURCE_IDS = SOURCES
BACKUP_NAME = re.compile(r"usage-\d{8}-\d{6}-\d{16,22}\.sqlite\Z")
QUERY_CACHE_SIZE = 6
SCHEMA = """
CREATE TABLE IF NOT EXISTS usage_records (
    id INTEGER PRIMARY KEY,
    source TEXT NOT NULL,
    stable_id TEXT,
    signature BLOB NOT NULL,
    session_id TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    project TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    reasoning_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    cache_write_tokens INTEGER NOT NULL,
    total_tokens INTEGER NOT NULL,
    cost REAL NOT NULL,
    UNIQUE(source, stable_id)
);
CREATE INDEX IF NOT EXISTS usage_signature_idx ON usage_records(source, signature, stable_id);
CREATE INDEX IF NOT EXISTS usage_unbound_identity_idx ON usage_records(source, session_id, timestamp, model, project) WHERE stable_id IS NULL;
CREATE INDEX IF NOT EXISTS usage_timestamp_idx ON usage_records(timestamp DESC, id DESC);
CREATE INDEX IF NOT EXISTS usage_source_timestamp_idx ON usage_records(source, timestamp);
CREATE INDEX IF NOT EXISTS usage_model_timestamp_idx ON usage_records(model, timestamp);
CREATE INDEX IF NOT EXISTS usage_project_timestamp_idx ON usage_records(project, timestamp);
CREATE TABLE IF NOT EXISTS usage_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"""


def _record_values(record: dict[str, Any]) -> tuple:
    values: list[Any] = []
    for name in FIELDS:
        value = record.get(name)
        if name in ("source", "session_id", "project", "model"):
            values.append(str(value or ""))
        elif name == "cost":
            cost = float(value or 0)
            if not math.isfinite(cost):
                raise ValueError("用量记录包含无效成本")
            values.append(cost)
        else:
            values.append(int(value or 0))
    if not values[0]:
        raise ValueError("用量记录缺少来源")
    return tuple(values)


def _signature(values: tuple) -> bytes:
    return hashlib.sha256(json.dumps(values, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8")).digest()


def _metadata(conn: sqlite3.Connection) -> dict:
    row = conn.execute("SELECT value FROM usage_metadata WHERE key='dashboard'").fetchone()
    return json.loads(row[0]) if row else {"revision": 1, "scanned_at": 0, "record_count": 0, "source_status": {}, "source_paths": {}, "source_errors": {}}


def _write_metadata(conn: sqlite3.Connection, payload: dict) -> None:
    conn.execute("INSERT OR REPLACE INTO usage_metadata(key,value) VALUES('dashboard',?)", (json.dumps(payload, ensure_ascii=False, separators=(",", ":")),))


class UsageStore:
    def __init__(self, path: Path, backup_dir: Path):
        self.path = path
        self.backup_dir = backup_dir
        self._write_lock = threading.RLock()
        self._query_cache_lock = threading.Lock()
        self._query_cache: OrderedDict[str, dict] = OrderedDict()

    def connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.path, timeout=30)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA busy_timeout=30000")
        return conn

    def initialize(self, snapshot_path: Path, codex_cache: Path | None = None, claude_cache: Path | None = None) -> dict:
        """Create the ledger atomically; failure never replaces the legacy data."""
        with self._write_lock:
            if self.path.exists():
                with closing(self.connect()) as conn:
                    if conn.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                        raise RuntimeError("用量数据库完整性检查失败，已停止加载")
                    conn.executescript(SCHEMA)
                    conn.execute("PRAGMA journal_mode=WAL")
                    return _metadata(conn)
            self.path.parent.mkdir(parents=True, exist_ok=True)
            temp_fd, temp_name = tempfile.mkstemp(prefix=".usage-migration-", suffix=".sqlite", dir=self.path.parent)
            os.close(temp_fd)
            temp = Path(temp_name)
            try:
                conn = sqlite3.connect(temp)
                try:
                    conn.executescript(SCHEMA)
                    conn.execute("BEGIN IMMEDIATE")
                    payload: dict = {}
                    expected_count = 0
                    expected_totals = [0] * len(NUMERIC_FIELDS)
                    if snapshot_path.exists():
                        # Preserve the exact migration inputs, including stable-ID caches.
                        backup = self.backup_dir / f"migration-{time.strftime('%Y%m%d-%H%M%S')}-{os.getpid()}"
                        backup.mkdir(parents=True, exist_ok=False)
                        for source_path in (snapshot_path, codex_cache, claude_cache):
                            if source_path and source_path.exists():
                                shutil.copy2(source_path, backup / source_path.name)
                        raw = json.loads((backup / snapshot_path.name).read_text(encoding="utf-8"))
                        payload = raw.get("dashboard", raw)
                        records = payload.get("records")
                        if not isinstance(records, list):
                            raise ValueError("原用量快照缺少记录，迁移已停止")
                        insert = f"INSERT INTO usage_records(stable_id,signature,{','.join(FIELDS)}) VALUES({','.join('?' for _ in range(len(FIELDS) + 2))})"
                        for record in records:
                            values = _record_values(record)
                            conn.execute(insert, (record.get("record_id") or None, _signature(values), *values))
                            expected_count += 1
                            for i, name in enumerate(NUMERIC_FIELDS):
                                expected_totals[i] += int(record.get(name) or 0)
                        actual = conn.execute("SELECT COUNT(*), " + ",".join(f"COALESCE(SUM({name}),0)" for name in NUMERIC_FIELDS) + " FROM usage_records").fetchone()
                        if tuple(actual) != (expected_count, *expected_totals):
                            raise RuntimeError("用量快照迁移校验失败，原文件未修改")
                        del records, raw
                        if codex_cache and (backup / codex_cache.name).exists():
                            self._bind_codex_cache(conn, backup / codex_cache.name)
                        if claude_cache and (backup / claude_cache.name).exists():
                            self._bind_claude_cache(conn, backup / claude_cache.name)
                    metadata = {key: payload.get(key, {}) for key in ("source_status", "source_paths", "source_errors")}
                    metadata.update({"revision": int(payload.get("revision") or int(payload.get("scanned_at", 0)) * 1000 or 1),
                                     "scanned_at": int(payload.get("scanned_at", 0)), "record_count": expected_count,
                                     "identity_synced": []})
                    _write_metadata(conn, metadata)
                    conn.commit()
                    if conn.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                        raise RuntimeError("迁移后的用量数据库完整性检查失败")
                finally:
                    conn.close()
                # A second process must never replace an already published ledger.
                os.link(temp, self.path)
                with closing(self.connect()) as ready:
                    ready.execute("PRAGMA journal_mode=WAL")
                return metadata
            finally:
                temp.unlink(missing_ok=True)

    @staticmethod
    def _bind(conn: sqlite3.Connection, source: str, stable_id: str, record: dict) -> None:
        values = _record_values(record)
        if values[0] != source:
            return
        try:
            conn.execute("UPDATE usage_records SET stable_id=? WHERE id=(SELECT id FROM usage_records WHERE source=? AND signature=? AND stable_id IS NULL ORDER BY id LIMIT 1)", (stable_id, source, _signature(values)))
        except sqlite3.IntegrityError:
            # The same upstream request can occur in multiple archived files.
            pass

    def _bind_codex_cache(self, conn: sqlite3.Connection, path: Path) -> None:
        cache = json.loads(path.read_text(encoding="utf-8"))
        for entry in cache.get("entries", {}).values():
            for stable_id, record in zip(entry.get("request_ids", []), entry.get("records", [])):
                if stable_id:
                    self._bind(conn, "codex", str(stable_id), record)

    def _bind_claude_cache(self, conn: sqlite3.Connection, path: Path) -> None:
        cache = json.loads(path.read_text(encoding="utf-8"))
        for entry in cache.get("files", {}).values():
            for message in entry.get("messages", []):
                if message.get("id") and isinstance(message.get("record"), dict):
                    self._bind(conn, "claude", f"claude_message:{message['id']}", message["record"])

    def status(self) -> dict:
        with closing(self.connect()) as conn:
            return _metadata(conn)

    def mark_pending(self, sources: list[str]) -> None:
        # If the collector updates its file cache and the process crashes before
        # the ledger commits, the next attempt must request a full source replay.
        with self._write_lock, closing(self.connect()) as conn:
            conn.execute("BEGIN IMMEDIATE")
            metadata = _metadata(conn)
            metadata["pending_sources"] = sorted(set(metadata.get("pending_sources", [])) | set(sources))
            _write_metadata(conn, metadata)
            conn.commit()

    @staticmethod
    def _filter(filters: dict) -> tuple[str, list[Any]]:
        clauses: list[str] = []
        args: list[Any] = []
        for field in ("source", "model", "project"):
            value = filters.get(field)
            if value and value != "all":
                clauses.append(f"r.{field}=?")
                args.append(value)
        hidden = [value for value in filters.get("hidden", []) if value in SOURCE_IDS]
        if hidden:
            clauses.append("r.source NOT IN (" + ",".join("?" for _ in hidden) + ")")
            args.extend(hidden)
        for key, operator in (("start", ">="), ("end", "<=")):
            if filters.get(key) is not None:
                clauses.append(f"r.timestamp {operator} ?")
                args.append(int(filters[key]))
        query = str(filters.get("query") or "").strip().lower()
        if query:
            clauses.append("instr(lower(r.project || ' ' || r.model || ' ' || r.source), ?) > 0")
            args.append(query)
        return (" AND ".join(clauses) if clauses else "1=1"), args

    @staticmethod
    def _prepare_rates(conn: sqlite3.Connection, prices: list[dict], models: list[str]) -> None:
        conn.execute("CREATE TEMP TABLE model_rates (model TEXT PRIMARY KEY, input REAL, output REAL, cache_read REAL, cache_write REAL)")
        by_name = {str(price.get("model_id", "")).lower(): price for price in prices}
        rows = []
        for model in models:
            name = model.lower()
            price = by_name.get(name) or by_name.get(name.rsplit("/", 1)[-1])
            if price:
                rows.append((model, price.get("input", 0), price.get("output", 0), price.get("cache_read"), price.get("cache_write")))
        conn.executemany("INSERT INTO model_rates VALUES (?,?,?,?,?)", rows)

    def query(self, filters: dict, prices: list[dict], *, include_board: bool = True) -> dict:
        """Return aggregates, chart buckets, and one detail page, not raw history."""
        where, args = self._filter(filters)
        page = max(1, int(filters.get("page") or 1))
        page_size = max(1, min(200, int(filters.get("page_size") or 20)))
        now = int(filters.get("now") or time.time())
        with closing(self.connect()) as conn:
            conn.execute("BEGIN")
            metadata = _metadata(conn)
            # Overview aggregates follow the ledger revision, filters, prices and
            # local calendar day, not the wall-clock second of each page load.
            calendar_start = conn.execute("SELECT date(?,'unixepoch','localtime','-364 days')", (now,)).fetchone()[0]
            cache_filters = {key: value for key, value in filters.items() if key not in ("page", "page_size", "now")}
            cache_key = json.dumps([metadata.get("revision", 1), cache_filters, calendar_start, prices], ensure_ascii=False, sort_keys=True, separators=(",", ":"))
            with self._query_cache_lock:
                cached = self._query_cache.get(cache_key)
                if cached is not None:
                    self._query_cache.move_to_end(cache_key)
            models = cached["models"] if cached else [row[0] for row in conn.execute("SELECT DISTINCT model FROM usage_records WHERE model<>'' ORDER BY model COLLATE NOCASE")]
            self._prepare_rates(conn, prices, models)
            joins = " FROM usage_records r LEFT JOIN model_rates p ON p.model=r.model "
            priced = "(r.cost>0 OR (p.model IS NOT NULL AND (r.cache_read_tokens<=0 OR p.cache_read IS NOT NULL) AND (r.cache_write_tokens<=0 OR p.cache_write IS NOT NULL)))"
            fresh = "CASE WHEN r.source IN ('trae','trae-cn') THEN MAX(0,r.input_tokens-r.cache_read_tokens-r.cache_write_tokens) ELSE MAX(0,r.input_tokens) END"
            cost = f"CASE WHEN r.cost>0 THEN r.cost WHEN NOT {priced} THEN 0 ELSE (({fresh})*p.input+MAX(0,r.output_tokens)*p.output+MAX(0,r.cache_read_tokens)*COALESCE(p.cache_read,0)+MAX(0,r.cache_write_tokens)*COALESCE(p.cache_write,0))/1000000.0 END"
            def detail_page(count: int) -> tuple[int, list[dict]]:
                current = min(page, max(1, math.ceil(count / page_size)))
                selected = ",".join(f"r.{name}" for name in FIELDS if name != "cost")
                rows = conn.execute(f"SELECT r.id AS row_id, {selected}, ({cost}) cost, CASE WHEN r.cost>0 THEN 'reported' WHEN {priced} THEN 'estimated' ELSE 'unpriced' END cost_status" + joins + f"WHERE {where} ORDER BY r.timestamp DESC,r.id DESC LIMIT ? OFFSET ?", [*args, page_size, (current - 1) * page_size])
                return current, [dict(row) for row in rows]
            def with_details(summary: dict) -> dict:
                current, detail = detail_page(summary["record_count"])
                board = []
                if include_board:
                    # The OBS rolling windows still use the exact requested second.
                    hidden_where, hidden_args = self._filter({"hidden": filters.get("hidden", [])})
                    periods = (3600, 86400, 7 * 86400, 30 * 86400, 90 * 86400, 365 * 86400)
                    expressions = ",".join("COALESCE(SUM(CASE WHEN r.timestamp>=? THEN r.total_tokens ELSE 0 END),0)" for _ in periods)
                    board = list(conn.execute(f"SELECT {expressions} FROM usage_records r WHERE {hidden_where}", [*(now - seconds for seconds in periods), *hidden_args]).fetchone())
                conn.commit()
                return {**summary, "board_totals": board, "page": current, "records": detail}

            if cached is not None:
                return with_details(cached)
            projects = [row[0] for row in conn.execute("SELECT DISTINCT project FROM usage_records WHERE project<>'' ORDER BY project")]
            aggregate = conn.execute(f"SELECT COUNT(*) AS count, MIN(r.timestamp) AS earliest, MAX(r.timestamp) AS latest, COALESCE(SUM(r.total_tokens),0) AS total, COALESCE(SUM(r.input_tokens),0) AS input, COALESCE(SUM(r.output_tokens),0) AS output, COALESCE(SUM(r.reasoning_tokens),0) AS reasoning, COALESCE(SUM(r.cache_read_tokens),0) AS cache, COALESCE(SUM({cost}),0) AS cost, COALESCE(SUM(CASE WHEN {priced} THEN 1 ELSE 0 END),0) AS priced" + joins + f"WHERE {where}", args).fetchone()
            count = aggregate["count"]
            totals = {name: aggregate[name] for name in ("total", "input", "output", "reasoning", "cache", "cost", "priced")}
            totals["unpriced"] = count - totals["priced"]
            start = filters.get("start") if filters.get("start") is not None else aggregate["earliest"]
            end = filters.get("end") if filters.get("end") is not None else aggregate["latest"]
            trend: list[dict] = []
            if start is not None and end is not None and start <= end:
                span = end - start
                if span <= 3600:
                    bucket, bucket_args = "CAST((r.timestamp-?)/60 AS INTEGER)", [start]
                elif span <= 86400:
                    bucket, bucket_args = "CAST((r.timestamp-?)/3600 AS INTEGER)", [start]
                elif span <= 366 * 86400:
                    bucket, bucket_args = "date(r.timestamp,'unixepoch','localtime')", []
                elif span <= 7 * 366 * 86400:
                    bucket, bucket_args = "date(r.timestamp,'unixepoch','localtime','weekday 0','-6 days')", []
                elif span <= 30 * 366 * 86400:
                    bucket, bucket_args = "strftime('%Y-%m',r.timestamp,'unixepoch','localtime')", []
                else:
                    bucket, bucket_args = "strftime('%Y',r.timestamp,'unixepoch','localtime')", []
                trend_rows = conn.execute(f"SELECT MIN(r.timestamp) timestamp, SUM(r.total_tokens) total_tokens, SUM(r.input_tokens) input_tokens, SUM(r.output_tokens) output_tokens, SUM(r.cache_read_tokens) cache_read_tokens, SUM({cost}) cost" + joins + f"WHERE {where} GROUP BY {bucket} ORDER BY timestamp", [*args, *bucket_args])
                trend = [dict(row) for row in trend_rows]
                # The chart chooses its granularity from its true endpoints.
                # A bucket's minimum alone would hide the latest event time.
                if filters.get("end") is None and aggregate["latest"] is not None:
                    trend.append({"timestamp": aggregate["latest"], "total_tokens": 0, "input_tokens": 0, "output_tokens": 0, "cache_read_tokens": 0, "cost": 0})
            cutoff = now - 366 * 86400
            # Date comparison uses local calendar days, including DST transitions.
            calendar_rows = conn.execute("SELECT MIN(r.timestamp) timestamp, SUM(r.total_tokens) total_tokens FROM usage_records r " + f"WHERE {where} AND r.timestamp>=? AND date(r.timestamp,'unixepoch','localtime')>=? GROUP BY date(r.timestamp,'unixepoch','localtime') ORDER BY timestamp", [*args, cutoff, calendar_start])
            calendar = [dict(row) for row in calendar_rows]
            groups: dict[str, list[dict]] = {}
            for field in ("project", "model"):
                rows = conn.execute(f"SELECT r.{field} label, COUNT(*) records, COALESCE(SUM(r.input_tokens),0) input_tokens, COALESCE(SUM(r.output_tokens),0) output_tokens, COALESCE(SUM(r.reasoning_tokens),0) reasoning_tokens, COALESCE(SUM(r.cache_read_tokens),0) cache_read_tokens, COALESCE(SUM(r.total_tokens),0) total_tokens FROM usage_records r WHERE {where} GROUP BY r.{field} ORDER BY total_tokens DESC, label", args)
                groups[field] = [dict(row) for row in rows]
            latest_record_id = conn.execute("SELECT COALESCE(MAX(id),0) FROM usage_records").fetchone()[0]
            summary = {"revision": metadata.get("revision", 1), "record_count": count, "latest_record_id": latest_record_id,
                       "totals": totals, "models": models, "projects": projects,
                       "board_groups": groups["project"], "model_groups": groups["model"],
                       "trend_records": trend, "calendar_records": calendar}
            with self._query_cache_lock:
                self._query_cache[cache_key] = summary
                self._query_cache.move_to_end(cache_key)
                if len(self._query_cache) > QUERY_CACHE_SIZE:
                    self._query_cache.popitem(last=False)
            return with_details(summary)

    def live_metrics(self, now: int | None = None) -> dict[str, int]:
        now = int(time.time()) if now is None else now
        periods = {"hour": 3600, "day": 86400, "week": 604800, "month": 2592000, "quarter": 7776000, "year": 31536000}
        expressions = ",".join("COALESCE(SUM(CASE WHEN timestamp>=? AND timestamp<=? THEN total_tokens ELSE 0 END),0)" for _ in periods)
        args = [item for seconds in periods.values() for item in (now - seconds, now)]
        with closing(self.connect()) as conn:
            row = conn.execute(f"SELECT {expressions} FROM usage_records WHERE timestamp>=? AND timestamp<=?", [*args, now - max(periods.values()), now]).fetchone()
            return dict(zip(periods, row))

    def all_records(self) -> Iterable[dict]:
        with closing(self.connect()) as conn:
            for row in conn.execute("SELECT " + ",".join(FIELDS) + " FROM usage_records ORDER BY id"):
                yield dict(row)

    def ingest(self, partial: dict, sources: list[str], *, fingerprints: dict[str, str] | None = None, cache_stamps: dict[str, str] | None = None) -> tuple[dict, bool]:
        """Upsert identified requests; never delete records for missing sources."""
        with self._write_lock, closing(self.connect()) as conn:
            conn.execute("BEGIN IMMEDIATE")
            metadata = _metadata(conn)
            changed = False
            count = int(metadata.get("record_count", 0))
            successful = set(metadata.get("identity_synced", []))
            statuses = partial.get("source_status", {})
            allowed = {source for source in sources if statuses.get(source) == "ready"}
            insert = f"INSERT INTO usage_records(stable_id,signature,{','.join(FIELDS)}) VALUES({','.join('?' for _ in range(len(FIELDS) + 2))})"
            update = ",".join(f"{name}=?" for name in FIELDS)
            seen_unidentified: dict[tuple[str, bytes], int] = {}
            for record in partial.get("records", []):
                if record.get("source") not in allowed:
                    continue
                values = _record_values(record)
                source = values[0]
                signature = _signature(values)
                stable_id = record.get("record_id")
                if stable_id:
                    stable_id = str(stable_id)
                    existing = conn.execute("SELECT id,signature FROM usage_records WHERE source=? AND stable_id=?", (source, stable_id)).fetchone()
                    if existing:
                        if existing["signature"] != signature:
                            conn.execute(f"UPDATE usage_records SET signature=?,{update} WHERE id=?", (signature, *values, existing["id"]))
                            changed = True
                        continue
                    old = conn.execute("SELECT id FROM usage_records WHERE source=? AND signature=? AND stable_id IS NULL ORDER BY id LIMIT 1", (source, signature)).fetchone()
                    if old:
                        conn.execute("UPDATE usage_records SET stable_id=? WHERE id=?", (stable_id, old["id"]))
                        continue
                    uncertain = conn.execute("SELECT 1 FROM usage_records WHERE source=? AND session_id=? AND timestamp=? AND project=? AND model=? AND stable_id IS NULL LIMIT 1", (source, values[1], values[2], values[3], values[4])).fetchone()
                    if uncertain:
                        raise RuntimeError("历史记录缺少可靠标识且内容发生变化；已保留原记录并暂停此来源同步")
                else:
                    # Unidentified records use a multiset count within this scan.
                    # They are bound below using a temporary occurrence counter.
                    stable_id = None
                    occurrence = seen_unidentified.get((source, signature), 0) + 1
                    seen_unidentified[(source, signature)] = occurrence
                    existing_count = conn.execute("SELECT COUNT(*) FROM usage_records WHERE source=? AND signature=? AND stable_id IS NULL", (source, signature)).fetchone()[0]
                    if existing_count >= occurrence:
                        continue
                conn.execute(insert, (stable_id, signature, *values))
                count += 1
                changed = True
            for source in sources:
                if statuses.get(source) in ("ready", "missing"):
                    successful.add(source)
                    for key, values in (("source_fingerprints", fingerprints), ("cache_stamps", cache_stamps)):
                        if values is not None and source in values:
                            metadata.setdefault(key, {})[source] = values[source]
            pending = set(metadata.get("pending_sources", []))
            pending.difference_update(source for source in sources if statuses.get(source) in ("ready", "missing"))
            metadata["pending_sources"] = sorted(pending)
            for field in ("source_status", "source_paths", "source_errors"):
                target = dict(metadata.get(field, {}))
                for source in sources:
                    if source in partial.get(field, {}):
                        value = partial[field][source]
                        if target.get(source) != value:
                            changed = True
                        target[source] = value
                metadata[field] = target
            metadata["identity_synced"] = sorted(successful)
            metadata["record_count"] = count
            metadata["scanned_at"] = int(time.time())
            if changed:
                metadata["revision"] = max(int(metadata.get("revision", 0)) + 1, int(time.time() * 1000))
            _write_metadata(conn, metadata)
            conn.commit()
            return metadata, changed

    def backup_if_due(self, interval_seconds: int = 86400) -> Path | None:
        with self._write_lock:
            self.backup_dir.mkdir(parents=True, exist_ok=True)
            backups = [path for path in self.backup_dir.glob("usage-*.sqlite") if BACKUP_NAME.fullmatch(path.name) and path.is_file()]
            if interval_seconds > 0 and backups and time.time() - max(path.stat().st_mtime for path in backups) < interval_seconds:
                return None
            name = f"usage-{time.strftime('%Y%m%d-%H%M%S')}-{time.time_ns()}.sqlite"
            destination = self.backup_dir / name
            fd, temp_name = tempfile.mkstemp(prefix=".usage-backup-", suffix=".sqlite", dir=self.backup_dir)
            os.close(fd)
            temporary = Path(temp_name)
            try:
                with closing(self.connect()) as source, closing(sqlite3.connect(temporary)) as target:
                    source.backup(target)
                    if target.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                        raise RuntimeError("用量备份完整性检查失败")
                os.rename(temporary, destination)
                # Rotate only backups created by this service, after the new
                # backup has been fully written and checked. Migration inputs
                # and all historical rows in the main database are untouched.
                previous = sorted(backups, key=lambda path: (path.stat().st_mtime_ns, path.name), reverse=True)
                for old in previous[9:]:
                    try:
                        old.unlink()
                    except OSError:
                        pass
                return destination
            finally:
                temporary.unlink(missing_ok=True)

    def backup_status(self) -> dict:
        times = []
        for path in self.backup_dir.glob("usage-*.sqlite"):
            if not BACKUP_NAME.fullmatch(path.name):
                continue
            try:
                if path.is_file():
                    times.append(path.stat().st_mtime)
            except OSError:
                continue
        return {"count": len(times), "last_backup_at": int(max(times, default=0))}
