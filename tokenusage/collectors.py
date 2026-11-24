from __future__ import annotations

import glob
import json
import os
import sqlite3
from pathlib import Path

from .models import UsageRecord


def _readonly_db(path: Path) -> sqlite3.Connection:
    return sqlite3.connect(f"file:{path.resolve().as_posix()}?mode=ro", uri=True)


def _project_name(path: str | None) -> str:
    if not path:
        return "unknown"
    return Path(path.replace("\\", "/")).name or path


def collect_codex(root: Path | None = None) -> list[UsageRecord]:
    root = root or Path.home() / ".codex"
    state_db = root / "state_5.sqlite"
    if state_db.exists():
        conn = _readonly_db(state_db)
        try:
            rows = conn.execute(
                "SELECT id, rollout_path, cwd, model, updated_at, tokens_used "
                "FROM threads WHERE tokens_used > 0"
            ).fetchall()
        finally:
            conn.close()
        records: list[UsageRecord] = []
        for session_id, rollout_path, cwd, model, updated_at, db_total in rows:
            parsed = _parse_codex_rollout(
                Path(rollout_path), str(session_id), _project_name(cwd), model or "unknown"
            )
            if parsed:
                records.extend(parsed)
            else:
                records.append(
                    UsageRecord(
                        source="codex",
                        session_id=session_id,
                        timestamp=int(updated_at or 0),
                        project=_project_name(cwd),
                        model=model or "unknown",
                        total_tokens=int(db_total or 0),
                    )
                )
        return records

    # Compatibility fallback for Codex versions without state_5.sqlite.
    files = glob.glob(str(root / "sessions" / "**" / "*.jsonl"), recursive=True)
    files += glob.glob(str(root / "archived_sessions" / "*.jsonl"))
    records_by_session: dict[str, UsageRecord] = {}

    for filename in files:
        session_id = Path(filename).stem
        project = "unknown"
        model = "unknown"
        last_usage: dict = {}
        last_timestamp = 0
        try:
            with open(filename, "rb") as handle:
                for line in handle:
                    # Rollout files contain very large prompt/tool payloads. Skip
                    # unrelated lines before JSON decoding to keep full-history
                    # scans practical.
                    if not any(marker in line for marker in (
                        b'"session_meta"', b'"turn_context"', b'"token_count"'
                    )):
                        continue
                    item = json.loads(line)
                    payload = item.get("payload") or {}
                    if item.get("type") == "session_meta":
                        session_id = payload.get("id") or payload.get("session_id") or session_id
                        project = _project_name(payload.get("cwd"))
                    elif item.get("type") == "turn_context":
                        model = payload.get("model") or model
                    elif item.get("type") == "event_msg" and payload.get("type") == "token_count":
                        usage = (payload.get("info") or {}).get("total_token_usage") or {}
                        if usage:
                            last_usage = usage
                            timestamp = item.get("timestamp") or ""
                            try:
                                from datetime import datetime

                                last_timestamp = int(datetime.fromisoformat(timestamp.replace("Z", "+00:00")).timestamp())
                            except ValueError:
                                pass
        except (OSError, json.JSONDecodeError):
            continue

        if last_usage:
            record = UsageRecord(
                    source="codex",
                    session_id=str(session_id),
                    timestamp=last_timestamp,
                    project=project,
                    model=model,
                    input_tokens=int(last_usage.get("input_tokens") or 0),
                    output_tokens=int(last_usage.get("output_tokens") or 0),
                    reasoning_tokens=int(last_usage.get("reasoning_output_tokens") or 0),
                    cache_read_tokens=int(last_usage.get("cached_input_tokens") or 0),
                    cache_write_tokens=int(last_usage.get("cache_write_input_tokens") or 0),
                    total_tokens=int(last_usage.get("total_tokens") or 0),
                ).normalized()
            previous = records_by_session.get(record.session_id)
            if previous is None or record.timestamp >= previous.timestamp:
                records_by_session[record.session_id] = record
    return list(records_by_session.values())


def _parse_codex_rollout(
    path: Path, session_id: str, project: str, model: str
) -> list[UsageRecord]:
    try:
        handle = path.open("r", encoding="utf-8")
    except OSError:
        return []
    records: list[UsageRecord] = []
    previous_total = 0
    with handle:
        for line in handle:
            if '"token_count"' not in line:
                continue
            try:
                item = json.loads(line)
            except json.JSONDecodeError:
                continue
            payload = item.get("payload") or {}
            if payload.get("type") != "token_count":
                continue
            info = payload.get("info") or {}
            cumulative = int((info.get("total_token_usage") or {}).get("total_tokens") or 0)
            if cumulative <= previous_total:
                continue
            previous_total = cumulative
            last = info.get("last_token_usage") or {}
            cached = int(last.get("cached_input_tokens") or 0)
            input_tokens = max(0, int(last.get("input_tokens") or 0) - cached)
            timestamp = 0
            try:
                from datetime import datetime

                timestamp = int(datetime.fromisoformat((item.get("timestamp") or "").replace("Z", "+00:00")).timestamp())
            except ValueError:
                pass
            records.append(
                UsageRecord(
                    source="codex",
                    session_id=session_id,
                    timestamp=timestamp,
                    project=project,
                    model=model,
                    input_tokens=input_tokens,
                    output_tokens=int(last.get("output_tokens") or 0),
                    reasoning_tokens=int(last.get("reasoning_output_tokens") or 0),
                    cache_read_tokens=cached,
                    cache_write_tokens=int(last.get("cache_write_input_tokens") or 0),
                    total_tokens=int(last.get("total_tokens") or 0),
                ).normalized()
            )
    return records


def collect_opencode(db_path: Path | None = None) -> list[UsageRecord]:
    db_path = db_path or Path.home() / ".local" / "share" / "opencode" / "opencode.db"
    if not db_path.exists():
        return []
    conn = _readonly_db(db_path)
    try:
        rows = conn.execute(
            """
            SELECT s.id, s.directory, s.model, s.time_updated, s.tokens_input,
                   s.tokens_output, s.tokens_reasoning, s.tokens_cache_read,
                   s.tokens_cache_write, s.cost, p.name, p.worktree
            FROM session s LEFT JOIN project p ON p.id = s.project_id
            """
        ).fetchall()
    finally:
        conn.close()

    records = []
    for row in rows:
        model = row[2] or "unknown"
        if model.startswith("{"):
            try:
                parsed = json.loads(model)
                model = parsed.get("modelID") or parsed.get("model") or parsed.get("id") or model
            except json.JSONDecodeError:
                pass
        records.append(
            UsageRecord(
                source="opencode",
                session_id=row[0],
                timestamp=int((row[3] or 0) / 1000),
                project=row[10] or _project_name(row[11] or row[1]),
                model=model,
                input_tokens=int(row[4] or 0),
                output_tokens=int(row[5] or 0),
                reasoning_tokens=int(row[6] or 0),
                cache_read_tokens=int(row[7] or 0),
                cache_write_tokens=int(row[8] or 0),
                cost=float(row[9] or 0),
            ).normalized()
        )
    return records


def collect_trae(db_path: Path, source: str) -> list[UsageRecord]:
    if not db_path.exists():
        return []
    conn = _readonly_db(db_path)
    try:
        project_rows = conn.execute(
            """
            SELECT sp.session_id, p.absolute_path
            FROM session_project sp LEFT JOIN project p ON p.project_id = sp.project_id
            """
        ).fetchall()
        projects = {sid: _project_name(path) for sid, path in project_rows}
        rows = conn.execute(
            "SELECT turn_id, session_id, created_at, context FROM chat_turn WHERE context IS NOT NULL"
        ).fetchall()
    finally:
        conn.close()

    records = []
    for turn_id, session_id, created_at, context_text in rows:
        try:
            context = json.loads(context_text)
        except (TypeError, json.JSONDecodeError):
            continue
        usage = context.get("token_usage") or {}
        if not usage or not usage.get("total_tokens"):
            continue
        model_info = (context.get("persist_user_message_context") or {}).get("model_info") or {}
        records.append(
            UsageRecord(
                source=source,
                session_id=turn_id or session_id,
                timestamp=int(created_at or 0),
                project=projects.get(session_id, "unknown"),
                model=model_info.get("config_name") or model_info.get("model_name") or "unknown",
                input_tokens=int(usage.get("prompt_tokens") or 0),
                output_tokens=int(usage.get("completion_tokens") or 0),
                reasoning_tokens=int(usage.get("reasoning_tokens") or 0),
                cache_read_tokens=int(usage.get("cache_read_input_tokens") or 0),
                cache_write_tokens=int(usage.get("cache_creation_input_tokens") or 0),
                total_tokens=int(usage.get("total_tokens") or 0),
            ).normalized()
        )
    return records


def default_trae_encrypted(edition: str) -> Path:
    folder = "Trae CN" if edition == "trae-cn" else "Trae"
    return Path(os.environ.get("APPDATA", "")) / folder / "ModularData" / "ai-agent" / "database.db"
