from __future__ import annotations

import csv
import gzip
import hashlib
import io
import json
import math
import os
import socket
import subprocess
import threading
import time
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

from .usage_store import UsageStore
from .sources import AGENT_SOURCES, NATIVE_SOURCES, SOURCES
from .bilibili_credentials import clear_bilibili_cookie, load_bilibili_cookie, sanitize_cookie, save_bilibili_cookie
from .bilibili_danmaku import BilibiliDanmakuService, validate_login_cookie
from .bilibili_login import BilibiliQrLogin, LoginUnavailable
from . import pricing

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / "dist"
DATA_ROOT = Path(os.environ.get("TOKENUSAGE_DATA_DIR", ROOT / "data"))
PORT = int(os.environ.get("TOKENUSAGE_PORT", "8787"))
DEFAULT_SYNC_INTERVAL = 60
SETTINGS_PATH = DATA_ROOT / "cache" / "source-settings.json"
SYNC_SETTINGS_PATH = DATA_ROOT / "cache" / "sync-settings.json"
BOARD_LAYOUT_PATH = DATA_ROOT / "cache" / "board-layout.json"
BOARD_SETTINGS_PATH = DATA_ROOT / "cache" / "board-settings.json"
BOARD_STYLE_PATH = DATA_ROOT / "cache" / "board-token-style.json"
BOARD_STYLE_PRESETS = ("classic", "minimal", "neon", "light")
DANMAKU_SETTINGS_PATH = DATA_ROOT / "cache" / "danmaku-settings.json"
BOARD_WIDGET_IDS = ("hour", "day", "week", "month", "quarter", "year", "danmaku")
DEFAULT_SOURCE_SETTINGS = {
    source: {"enabled": True, "interval_seconds": 60, "last_checked_at": 0, "last_changed_at": 0, "fingerprint": None}
    for source in SOURCES
}
_lock = threading.Lock()
_scan_lock = threading.Lock()
_scan_request_lock = threading.Lock()
_source_settings_lock = threading.RLock()
_sync_settings_lock = threading.RLock()
_presence_lock = threading.Lock()
_last_foreground_seen = 0.0
_encoding_lock = threading.Lock()
_initial_load_lock = threading.Lock()
_cached_payload: dict | None = None
_cached_records: list[dict] = []
_usage_store: UsageStore | None = None
_usage_backup_error: str | None = None
_cached_at = 0.0
_scan_thread: threading.Thread | None = None
_revision = 0
_encoded_revision = -1
_encoded_dashboard = b""
_encoded_dashboard_gzip = b""
_board_layout_lock = threading.Lock()
_board_layout_cache: dict | None = None
_board_layout_revision = 0
_board_settings_lock = threading.Lock()
_danmaku_lock = threading.Lock()
_danmaku_service = BilibiliDanmakuService()
_bilibili_qr = BilibiliQrLogin()
_danmaku_configured_room: int | None = None
_danmaku_configured_cookie = ""
_danmaku_font_size = 16


def _normalize_danmaku_font_size(value: object) -> int:
    try:
        return max(10, min(32, int(value)))
    except (TypeError, ValueError):
        return 16


def _load_danmaku_settings() -> dict:
    try:
        raw = json.loads(DANMAKU_SETTINGS_PATH.read_text(encoding="utf-8"))
        return {"room_id": max(0, int(raw.get("room_id", 0))), "font_size": _normalize_danmaku_font_size(raw.get("font_size", 16))}
    except (OSError, json.JSONDecodeError, TypeError, ValueError):
        return {"room_id": 0, "font_size": 16}


def _configure_danmaku(room_id: int | None, cookie: str | None = None, clear_cookie: bool = False, font_size: int | None = None) -> dict:
    global _danmaku_configured_room, _danmaku_configured_cookie, _danmaku_font_size
    with _danmaku_lock:
        current_settings = _load_danmaku_settings()
        room_id = current_settings['room_id'] if room_id is None else max(0, int(room_id))
        _danmaku_font_size = _normalize_danmaku_font_size(font_size if font_size is not None else current_settings["font_size"])
        login_cookie = load_bilibili_cookie()
        if clear_cookie:
            clear_bilibili_cookie()
            login_cookie = ""
        elif cookie is not None and cookie.strip():
            login_cookie = sanitize_cookie(cookie)
            validate_login_cookie(login_cookie)
            save_bilibili_cookie(login_cookie)
        _atomic_write_text(DANMAKU_SETTINGS_PATH, json.dumps({"room_id": room_id, "font_size": _danmaku_font_size}, ensure_ascii=False))
        _danmaku_service.configure(room_id, login_cookie)
        _danmaku_configured_room = room_id
        _danmaku_configured_cookie = login_cookie
    return {**_danmaku_service.snapshot(), "font_size": _danmaku_font_size}


def _danmaku_snapshot() -> dict:
    global _danmaku_configured_room, _danmaku_configured_cookie, _danmaku_font_size
    with _danmaku_lock:
        settings = _load_danmaku_settings()
        room_id = settings["room_id"]
        _danmaku_font_size = settings["font_size"]
        if _danmaku_configured_room != room_id:
            _danmaku_configured_cookie = load_bilibili_cookie()
            _danmaku_service.configure(room_id, _danmaku_configured_cookie)
            _danmaku_configured_room = room_id
    return {**_danmaku_service.snapshot(), "font_size": _danmaku_font_size}


def _default_board_layout() -> dict:
    # Reference desktop preset (1810 × 952); revision 0 is rendered responsively by the browser.
    token_width = (1810 - 10 - 297 - 7 * 7) / 6
    return {
        "revision": 0,
        "updated_at": 0,
        "viewport": {"width": 1810, "height": 952},
        "cards": [
            {"x": 5 + index * (token_width + 7), "y": 13, "width": token_width, "height": 82}
            for index in range(6)
        ] + [{"x": 1508, "y": 13, "width": 297, "height": 926}],
    }


def _normalize_board_settings(value: object) -> dict:
    visible = value.get("visible") if isinstance(value, dict) else None
    if not isinstance(visible, list):
        raise ValueError("看板显示设置必须包含 visible 列表")
    selected = {str(item) for item in visible}
    order = value.get("order", list(BOARD_WIDGET_IDS))
    if not isinstance(order, list):
        raise ValueError("看板来源顺序必须是列表")
    ranked = list(dict.fromkeys(item for item in order if item in BOARD_WIDGET_IDS))
    return {
        "visible": [widget_id for widget_id in BOARD_WIDGET_IDS if widget_id in selected],
        "order": ranked + [widget_id for widget_id in BOARD_WIDGET_IDS if widget_id not in ranked],
    }


def _load_board_settings() -> dict:
    try:
        raw = json.loads(BOARD_SETTINGS_PATH.read_text(encoding="utf-8"))
        return {**_normalize_board_settings(raw), "revision": max(0, int(raw.get("revision", 0)))}
    except (OSError, json.JSONDecodeError, TypeError, ValueError):
        return {"visible": list(BOARD_WIDGET_IDS), "order": list(BOARD_WIDGET_IDS), "revision": 0}


def _save_board_settings(value: object) -> dict:
    normalized = _normalize_board_settings(value)
    with _board_settings_lock:
        current = _load_board_settings()
        if "order" not in value:
            normalized["order"] = current["order"]
        payload = {**normalized, "revision": max(current["revision"] + 1, int(time.time() * 1000))}
        _atomic_write_text(BOARD_SETTINGS_PATH, json.dumps(payload, ensure_ascii=False, separators=(",", ":")))
    return payload


def _normalize_board_style(value: object) -> dict:
    if not isinstance(value, dict) or value.get("preset") not in BOARD_STYLE_PRESETS:
        raise ValueError("未知的 Token 样式预设")
    css = value.get("css", "")
    if not isinstance(css, str) or len(css) > 4000:
        raise ValueError("CSS 必须是 4000 字符以内的文本")
    return {"preset": value["preset"], "css": css}


def _load_board_style() -> dict:
    try:
        return _normalize_board_style(json.loads(BOARD_STYLE_PATH.read_text(encoding="utf-8")))
    except (OSError, ValueError):
        return {"preset": "classic", "css": ""}


def _normalize_board_layout(value: dict) -> dict:
    viewport = value.get("viewport") if isinstance(value, dict) else None
    cards = value.get("cards") if isinstance(value, dict) else None
    if not isinstance(viewport, dict) or not isinstance(cards, list) or len(cards) != 7:
        raise ValueError("看板布局必须包含 viewport 和 7 张卡片")
    width = float(viewport.get("width", 0))
    height = float(viewport.get("height", 0))
    if not (1 <= width <= 10000 and 1 <= height <= 10000):
        raise ValueError("看板画布尺寸无效")
    normalized_cards = []
    for card in cards:
        if not isinstance(card, dict):
            raise ValueError("看板卡片布局无效")
        numbers = {key: float(card.get(key, float("nan"))) for key in ("x", "y", "width", "height")}
        if not all(number == number and abs(number) <= 10000 for number in numbers.values()):
            raise ValueError("看板卡片坐标无效")
        if numbers["width"] < 64 or numbers["height"] < 32:
            raise ValueError("看板卡片尺寸过小")
        normalized_cards.append({key: round(number, 3) for key, number in numbers.items()})
    return {"viewport": {"width": round(width, 3), "height": round(height, 3)}, "cards": normalized_cards}


def _load_board_layout() -> dict:
    global _board_layout_cache, _board_layout_revision
    with _board_layout_lock:
        if _board_layout_cache is not None:
            return json.loads(json.dumps(_board_layout_cache))
        try:
            raw = json.loads(BOARD_LAYOUT_PATH.read_text(encoding="utf-8"))
            upgraded = isinstance(raw.get("cards"), list) and len(raw["cards"]) == 6
            if upgraded:
                raw["viewport"] = {**raw.get("viewport", {}), "height": max(float(raw.get("viewport", {}).get("height", 0)), 520)}
                raw["cards"] = [*raw["cards"], {"x": 20, "y": 220, "width": 810, "height": 280}]
            normalized = _normalize_board_layout(raw)
            _board_layout_revision = max(int(raw.get("revision", 0)), int(time.time() * 1000) if upgraded else 0)
            _board_layout_cache = {**normalized, "revision": _board_layout_revision, "updated_at": int(raw.get("updated_at", 0))}
            if upgraded:
                _board_layout_cache["updated_at"] = _board_layout_revision
                _atomic_write_text(BOARD_LAYOUT_PATH, json.dumps(_board_layout_cache, ensure_ascii=False, separators=(",", ":")))
        except (OSError, json.JSONDecodeError, TypeError, ValueError):
            _board_layout_cache = _default_board_layout()
        return json.loads(json.dumps(_board_layout_cache))


def _save_board_layout(value: dict) -> dict:
    global _board_layout_cache, _board_layout_revision
    normalized = _normalize_board_layout(value)
    with _board_layout_lock:
        _board_layout_revision = max(_board_layout_revision + 1, int(time.time() * 1000))
        payload = {**normalized, "revision": _board_layout_revision, "updated_at": int(time.time() * 1000)}
        _atomic_write_text(BOARD_LAYOUT_PATH, json.dumps(payload, ensure_ascii=False, separators=(",", ":")))
        _board_layout_cache = payload
        return json.loads(json.dumps(payload))


def _next_revision() -> int:
    return max(_revision + 1, int(time.time() * 1000))


def _install_payload(payload: dict, records: list[dict], *, bump_revision: bool) -> None:
    global _cached_at, _cached_payload, _cached_records, _revision
    global _encoded_revision, _encoded_dashboard, _encoded_dashboard_gzip
    if bump_revision:
        _revision = _next_revision()
    elif not _revision:
        _revision = int(payload.get("revision") or int(payload.get("scanned_at", 0)) * 1000 or 1)
    payload["revision"] = _revision
    _cached_payload, _cached_records, _cached_at = payload, records, time.monotonic()
    _encoded_revision = -1
    _encoded_dashboard = b""
    _encoded_dashboard_gzip = b""


def _dashboard_status(payload: dict | None = None) -> dict:
    if payload is None:
        payload, _ = _collect()
    result = {
        "revision": payload.get("revision", _revision),
        "scanned_at": payload.get("scanned_at", 0),
        "record_count": payload.get("record_count", len(payload.get("records", []))),
        "source_status": payload.get("source_status", {}),
        "source_paths": payload.get("source_paths", {}),
        "source_errors": payload.get("source_errors", {}),
    }
    if _usage_store is not None:
        result["backup"] = {**_usage_store.backup_status(), "error": _usage_backup_error}
    return result


def _encoded_dashboard_response() -> tuple[bytes, bytes, int]:
    global _encoded_revision, _encoded_dashboard, _encoded_dashboard_gzip
    with _encoding_lock:
        while True:
            payload, _ = _collect()
            with _lock:
                revision = int(payload.get("revision", _revision))
                if _encoded_revision == revision:
                    return _encoded_dashboard, _encoded_dashboard_gzip, revision
            raw = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode()
            compressed = gzip.compress(raw, compresslevel=6)
            with _lock:
                if int((_cached_payload or {}).get("revision", _revision)) == revision:
                    _encoded_revision = revision
                    _encoded_dashboard = raw
                    _encoded_dashboard_gzip = compressed
                    return raw, compressed, revision


def _load_snapshot() -> tuple[dict, list[dict]] | None:
    path = ROOT / "data" / "cache" / "dashboard.json"
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
        payload = raw.get("dashboard", raw)
        records = payload.get("records")
        if not isinstance(records, list):
            return None
        return payload, records
    except (OSError, json.JSONDecodeError, TypeError):
        return None


def _atomic_write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.{threading.get_ident()}.tmp")
    temporary.write_text(text, encoding="utf-8")
    try:
        for attempt in range(5):
            try:
                os.replace(temporary, path)
                return
            except PermissionError:
                if attempt == 4:
                    raise
                time.sleep(0.05 * (attempt + 1))
    finally:
        try:
            temporary.unlink(missing_ok=True)
        except OSError:
            pass


def _save_snapshot(payload: dict) -> None:
    path = ROOT / "data" / "cache" / "dashboard.json"
    _atomic_write_text(path, json.dumps({"version": 2, "dashboard": payload}, ensure_ascii=False))


def _load_settings() -> dict:
    try:
        raw = json.loads(SETTINGS_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError, TypeError):
        raw = {}
    settings = {}
    for source in SOURCES:
        item = {**DEFAULT_SOURCE_SETTINGS[source], **(raw.get(source, {}) if isinstance(raw, dict) else {})}
        item["enabled"] = bool(item.get("enabled", True))
        item["interval_seconds"] = max(0, int(item.get("interval_seconds", 60)))
        settings[source] = item
    return settings


def _save_settings(settings: dict) -> None:
    _atomic_write_text(SETTINGS_PATH, json.dumps(settings, ensure_ascii=False, indent=2))


def _save_scan_metadata(settings: dict, sources: list[str]) -> None:
    # A scan may take minutes. Do not overwrite settings changed while it ran.
    with _source_settings_lock:
        current = _load_settings()
        for source in sources:
            for key in ("last_checked_at", "last_changed_at", "fingerprint"):
                current[source][key] = settings[source].get(key)
        _save_settings(current)


def _load_sync_settings() -> dict:
    try:
        raw = json.loads(SYNC_SETTINGS_PATH.read_text(encoding="utf-8"))
        if not isinstance(raw, dict):
            raw = {}
    except (OSError, json.JSONDecodeError, TypeError):
        raw = {}
    interval = raw.get("interval_seconds")
    return {"auto_scan_enabled": raw.get("auto_scan_enabled", True) is not False,
            "interval_seconds": interval if type(interval) is int and interval in (5, 10, 30, 60, 300, 600) else DEFAULT_SYNC_INTERVAL}


def _update_sync_settings(value: dict) -> dict:
    with _sync_settings_lock:
        settings = _load_sync_settings()
        if "auto_scan_enabled" in value:
            if not isinstance(value["auto_scan_enabled"], bool):
                raise ValueError("自动扫描设置无效")
            settings["auto_scan_enabled"] = value["auto_scan_enabled"]
        if "interval_seconds" in value:
            interval = value["interval_seconds"]
            if interval is not None and (type(interval) is not int or interval not in (5, 10, 30, 60, 300, 600)):
                raise ValueError("扫描间隔无效")
            settings["interval_seconds"] = DEFAULT_SYNC_INTERVAL if interval is None else interval
        _atomic_write_text(SYNC_SETTINGS_PATH, json.dumps(settings, ensure_ascii=False, indent=2))
        return settings


def _update_source_settings(requested: dict) -> dict:
    with _source_settings_lock:
        settings = _load_settings()
        for source in SOURCES:
            incoming = requested.get(source)
            if not isinstance(incoming, dict):
                continue
            if "enabled" in incoming:
                settings[source]["enabled"] = bool(incoming["enabled"])
            if "interval_seconds" in incoming:
                settings[source]["interval_seconds"] = max(0, min(int(incoming["interval_seconds"]), 86400))
        _save_settings(settings)
        return settings


def _source_paths() -> dict[str, Path]:
    appdata = Path(os.environ.get("APPDATA", Path.home() / "AppData" / "Roaming"))
    return {
        "codex": Path.home() / ".codex",
        "claude": Path(os.environ.get("CLAUDE_CONFIG_DIR", Path.home() / ".claude")) / "projects",
        "trae-cn": appdata / "Trae CN" / "ModularData" / "ai-agent" / "database.db",
        "trae": appdata / "Trae" / "ModularData" / "ai-agent" / "database.db",
        "opencode": Path.home() / ".local" / "share" / "opencode" / "opencode.db",
    }


def _collector_executable() -> Path:
    name = "dashboard-json.exe" if os.name == "nt" else "dashboard-json"
    release = ROOT / "src-tauri" / "target" / "release" / name
    debug = ROOT / "src-tauri" / "target" / "debug" / name
    override = os.environ.get("TOKENUSAGE_COLLECTOR")
    executable = Path(override) if override else release if release.exists() else debug
    if not executable.exists():
        raise RuntimeError("未找到 Rust 采集器，请先运行 start-web.cmd")
    return executable


def _agent_fingerprints(requested: list[str]) -> dict[str, str]:
    agents = [source for source in requested if source in AGENT_SOURCES]
    if not agents:
        return {}
    result = subprocess.run([str(_collector_executable()), "--source-fingerprints", "--sources", ",".join(agents)],
                            cwd=ROOT, capture_output=True, text=True, encoding="utf-8", timeout=120)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "用量来源检查失败")
    payload = json.loads(result.stdout)
    fingerprints = {}
    for source in agents:
        entry = payload.get(source, {})
        if "Ok" in entry:
            fingerprints[source] = entry["Ok"]
        elif "Err" in entry:
            # Force the collector to run so this source reports its error without
            # suppressing other sources in the same scan.
            fingerprints[source] = f"unreadable:{time.time_ns()}"
        else:
            raise RuntimeError("用量采集器版本过旧，请重新构建后再同步")
    return fingerprints


def _source_fingerprint(source: str) -> str:
    path = _source_paths()[source]
    if source in ("codex", "claude"):
        roots = [path / "sessions", path / "archived_sessions"] if source == "codex" else [path]
        digest = hashlib.sha256(b"session-files-v2")
        for root in roots:
            digest.update(str(root).encode("utf-8", "surrogatepass"))
            if not root.exists():
                digest.update(b"missing")
                continue
            names = []
            def onerror(error: OSError) -> None:
                raise error
            for directory, dirs, files in os.walk(root, onerror=onerror, followlinks=False):
                dirs.sort()
                for name in files:
                    if name.endswith(".jsonl") or (source == "codex" and name.endswith(".jsonl.zst")):
                        names.append(Path(directory) / name)
            for candidate in sorted(names):
                try:
                    stat = candidate.stat()
                    digest.update(f"{candidate}:{stat.st_size}:{stat.st_mtime_ns}\n".encode("utf-8", "surrogatepass"))
                except FileNotFoundError:
                    digest.update(f"{candidate}:disappeared\n".encode("utf-8", "surrogatepass"))
        return digest.hexdigest()
    parts = []
    for candidate in (path, Path(f"{path}-wal")):
        try:
            stat = candidate.stat()
            parts.append(f"{candidate.name}:{stat.st_size}:{stat.st_mtime_ns}")
        except OSError:
            parts.append(f"{candidate.name}:missing")
    return ("messages-v3|" if source == "opencode" else "") + "|".join(parts)


def _collector_cache_stamp(source: str) -> str | None:
    prefix = {"codex": "codex-rollouts", "claude": "claude-sessions"}.get(source)
    if not prefix:
        return None
    cache = Path(os.environ.get("TOKENUSAGE_CACHE_DIR", DATA_ROOT / "cache"))
    if os.environ.get("TOKENUSAGE_DATA_DIR"):
        cache = DATA_ROOT / "cache"
    digest = hashlib.sha256()
    for path in sorted(cache.glob(f"{prefix}*.json")):
        try:
            stat = path.stat()
            digest.update(f"{path.name}:{stat.st_size}:{stat.st_mtime_ns}\n".encode())
        except FileNotFoundError:
            digest.update(f"{path.name}:missing\n".encode())
    return digest.hexdigest()


def _collect() -> tuple[dict, list[dict]]:
    if _usage_store is not None:
        return _usage_store.status(), []
    with _lock:
        if _cached_payload is not None:
            return _cached_payload, _cached_records
        snapshot = _load_snapshot()
        if snapshot:
            _install_payload(snapshot[0], snapshot[1], bump_revision=False)
            return _cached_payload, _cached_records
    with _initial_load_lock:
        with _lock:
            if _cached_payload is not None:
                return _cached_payload, _cached_records
            if not _load_sync_settings()["auto_scan_enabled"]:
                payload = {"records": [], "scanned_at": 0, "source_status": {source: "missing" for source in SOURCES}, "source_paths": {}, "source_errors": {}}
                _install_payload(payload, [], bump_revision=False)
                return _cached_payload, _cached_records
        return _collect_fresh(list(SOURCES), force=True)


def _collect_fresh(sources: list[str] | None = None, force: bool = False, rebuild_codex: bool = False) -> tuple[dict, list[dict]]:
    if _usage_store is not None:
        return _collect_fresh_store(sources, force, rebuild_codex)
    requested = [source for source in (sources or list(SOURCES)) if source in SOURCES]
    if not requested:
        return _collect()
    with _lock:
        # An empty placeholder created while auto-scan was off is not a snapshot.
        if _cached_payload is not None and _cached_payload.get("scanned_at") == 0:
            force = True
    settings = _load_settings()
    changed = []
    fingerprints = {}
    now = int(time.time())
    agent_fingerprints = _agent_fingerprints(requested)
    for source in requested:
        fingerprint = agent_fingerprints[source] if source in AGENT_SOURCES else _source_fingerprint(source)
        fingerprints[source] = fingerprint
        if force or fingerprint != settings[source].get("fingerprint"):
            changed.append(source)
        settings[source]["last_checked_at"] = now
    if not changed:
        _save_scan_metadata(settings, requested)
        return _collect()
    executable = _collector_executable()
    command = [str(executable), "--sources", ",".join(changed)]
    if rebuild_codex and "codex" in changed:
        command.append("--rebuild-codex")
    with _scan_lock:
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", timeout=300, check=False)
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or "Rust dashboard collector failed")
        partial = json.loads(result.stdout)
        with _lock:
            if _cached_payload is None:
                snapshot = _load_snapshot()
                if snapshot:
                    _install_payload(snapshot[0], snapshot[1], bump_revision=False)
            first_install = _cached_payload is None
            base = _cached_payload or partial
            base_records = base.get("records", [])
            partial_records = partial.get("records", [])
            semantic_changes = []
            for source in changed:
                before = [record for record in base_records if record.get("source") == source]
                after = [record for record in partial_records if record.get("source") == source]
                if partial.get("source_status", {}).get(source) in ("error", "needs-permission"):
                    after = before
                    partial_records = [record for record in partial_records if record.get("source") != source] + before
                fields_changed = any(
                    base.get(field, {}).get(source) != partial.get(field, {}).get(source)
                    for field in ("source_status", "source_paths", "source_errors")
                )
                if first_install or before != after or fields_changed:
                    semantic_changes.append(source)
            if semantic_changes:
                retained = [record for record in base_records if record.get("source") not in semantic_changes]
                incoming_records = [record for record in partial_records if record.get("source") in semantic_changes]
                records = retained + incoming_records
                payload = {**base, "records": records, "scanned_at": now}
                for field in ("source_status", "source_paths", "source_errors"):
                    payload[field] = dict(base.get(field, {}))
                    incoming = partial.get(field, {})
                    for source in semantic_changes:
                        if source in incoming:
                            payload[field][source] = incoming[source]
                _install_payload(payload, records, bump_revision=True)
            else:
                payload, records = base, base_records
        for source in requested:
            if source not in changed or partial.get("source_status", {}).get(source) not in ("error", "needs-permission"):
                settings[source]["fingerprint"] = fingerprints[source]
        for source in semantic_changes:
            settings[source]["last_changed_at"] = now
        _save_scan_metadata(settings, requested)
        if semantic_changes:
            _save_snapshot(payload)
        return payload, records


def _collect_fresh_store(sources: list[str] | None, force: bool, rebuild_codex: bool) -> tuple[dict, list[dict]]:
    assert _usage_store is not None
    requested = [source for source in (sources or list(SOURCES)) if source in SOURCES]
    if not requested:
        return _usage_store.status(), []
    settings = _load_settings()
    state = _usage_store.status()
    synced = set(state.get("identity_synced", []))
    pending = set(state.get("pending_sources", []))
    saved_fingerprints = state.get("source_fingerprints", {})
    saved_cache_stamps = state.get("cache_stamps", {})
    current_cache_stamps = {source: stamp for source in requested if (stamp := _collector_cache_stamp(source)) is not None}
    stale_cache = {source for source, stamp in current_cache_stamps.items() if saved_cache_stamps.get(source) != stamp}
    fingerprints: dict[str, str] = {}
    changed = []
    now = int(time.time())
    agent_fingerprints = _agent_fingerprints(requested)
    for source in requested:
        fingerprint = agent_fingerprints[source] if source in AGENT_SOURCES else _source_fingerprint(source)
        fingerprints[source] = fingerprint
        if force or source not in synced or source in pending or source in stale_cache or fingerprint != saved_fingerprints.get(source):
            changed.append(source)
        settings[source]["last_checked_at"] = now
    if not changed:
        _save_scan_metadata(settings, requested)
        return _usage_store.status(), []
    executable = _collector_executable()
    command = [str(executable), "--sources", ",".join(changed)]
    if rebuild_codex and "codex" in changed:
        command.append("--rebuild-codex")
        _usage_store.backup_if_due(interval_seconds=0)
    if all(source in synced and source not in pending and source not in stale_cache for source in changed):
        command.append("--changes-only")
    environment = os.environ.copy()
    if os.environ.get("TOKENUSAGE_DATA_DIR"):
        environment["TOKENUSAGE_CACHE_DIR"] = str(DATA_ROOT / "cache")
    with _scan_lock:
        _usage_store.mark_pending(changed)
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", timeout=300, check=False, env=environment)
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or "Rust dashboard collector failed")
        partial = json.loads(result.stdout)
        if any(record.get("source") in NATIVE_SOURCES and not record.get("record_id") for record in partial.get("records", [])):
            raise RuntimeError("用量采集器版本过旧，请重新构建后再同步")
        new_cache_stamps = {source: stamp for source in changed if (stamp := _collector_cache_stamp(source)) is not None}
        payload, records_changed = _usage_store.ingest(partial, changed, fingerprints=fingerprints, cache_stamps=new_cache_stamps)
        for source in requested:
            if source not in changed or partial.get("source_status", {}).get(source) not in ("error", "needs-permission"):
                settings[source]["fingerprint"] = fingerprints[source]
        if records_changed:
            for source in changed:
                settings[source]["last_changed_at"] = now
        _save_scan_metadata(settings, requested)
        return payload, []


def _effective_scan_interval(sync_settings: dict, foreground: bool) -> int:
    return sync_settings["interval_seconds"] if foreground else 600


def _scheduler_loop() -> None:
    while True:
        settings = _load_settings()
        sync_settings = _load_sync_settings()
        now = int(time.time())
        for source in SOURCES:
            item = settings[source]
            with _presence_lock:
                foreground = time.monotonic() - _last_foreground_seen < 35
            interval = _effective_scan_interval(sync_settings, foreground)
            if sync_settings["auto_scan_enabled"] and item["enabled"] and interval > 0 and now - int(item.get("last_checked_at", 0)) >= interval:
                try:
                    if _scan_request_lock.acquire(blocking=False):
                        try:
                            _collect_fresh([source])
                        finally:
                            _scan_request_lock.release()
                except Exception as error:
                    print(f"TokenUsage: {source} scan failed: {error}")
        time.sleep(2)


def _scan_enabled_sources() -> None:
    if not _scan_request_lock.acquire(blocking=False):
        return
    try:
        if not _load_sync_settings()["auto_scan_enabled"]:
            return
        settings = _load_settings()
        sources = [source for source in SOURCES if settings[source]["enabled"]]
        if sources:
            _collect_fresh(sources)
    except Exception as error:
        print(f"TokenUsage: background sync failed: {error}")
    finally:
        _scan_request_lock.release()


def _mark_usage_foreground(opened: bool) -> None:
    global _last_foreground_seen
    with _presence_lock:
        _last_foreground_seen = time.monotonic()
    if opened and _load_sync_settings()["auto_scan_enabled"]:
        threading.Thread(target=_scan_enabled_sources, daemon=True, name="tokenusage-page-sync").start()


def _pricing_scheduler_loop() -> None:
    while True:
        try:
            pricing.sync(automatic=True)
        except Exception as error:
            print(f"TokenUsage: models.dev sync failed: {error}")
        time.sleep(300)


def _usage_backup_loop() -> None:
    global _usage_backup_error
    while True:
        try:
            if _usage_store is not None:
                _usage_store.backup_if_due()
                _usage_backup_error = None
        except Exception as error:
            _usage_backup_error = "自动备份失败，请检查磁盘空间或文件权限"
            print(f"TokenUsage: usage backup failed: {error}")
        time.sleep(3600)


class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(DIST), **kwargs)

    def _send(self, body: bytes, content_type: str, status: int = 200) -> None:
        if "gzip" in self.headers.get("Accept-Encoding", "") and len(body) > 1024:
            body = gzip.compress(body, compresslevel=6)
            encoding = "gzip"
        else:
            encoding = None
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        cache_control = "no-store" if self.path.startswith("/api/") else "private, max-age=10"
        self.send_header("Cache-Control", cache_control)
        self.send_header("Vary", "Accept-Encoding")
        if encoding:
            self.send_header("Content-Encoding", encoding)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            # A browser may cancel an outdated query while the next filter loads.
            pass

    def _send_dashboard(self) -> None:
        if _usage_store is not None:
            payload = {**_usage_store.status(), "records": list(_usage_store.all_records())}
            self._send(json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode(), "application/json; charset=utf-8")
            return
        raw, compressed, revision = _encoded_dashboard_response()
        use_gzip = "gzip" in self.headers.get("Accept-Encoding", "")
        etag = f'"{revision}-{"gzip" if use_gzip else "identity"}"'
        if self.headers.get("If-None-Match") == etag:
            self.send_response(304)
            self.send_header("ETag", etag)
            self.send_header("Cache-Control", "private, no-cache")
            self.end_headers()
            return
        body = compressed if use_gzip else raw
        self.send_response(200)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Cache-Control", "private, no-cache")
        self.send_header("ETag", etag)
        self.send_header("Vary", "Accept-Encoding")
        if use_gzip:
            self.send_header("Content-Encoding", "gzip")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _local_access_allowed(self) -> bool:
        # Browsers cannot use a foreign page to call the local API or read its settings.
        host = self.headers.get('Host', '').split(':', 1)[0].lower()
        origin = self.headers.get('Origin')
        if host not in ('127.0.0.1', 'localhost') or self.headers.get('Sec-Fetch-Site') == 'cross-site':
            return False
        if origin:
            try:
                parsed = urlparse(origin)
                valid = parsed.scheme in ('http', 'https') and parsed.hostname in ('127.0.0.1', 'localhost') and parsed.port in (self.server.server_address[1], 1420)
            except ValueError:
                valid = False
            if not valid:
                return False
        return True

    def _send_json(self, payload: dict, status: int = 200) -> None:
        self._send(json.dumps(payload, ensure_ascii=False, separators=(',', ':')).encode('utf-8'), 'application/json; charset=utf-8', status)

    def _pricing_request(self, path: str) -> bool:
        if not path.startswith('/api/pricing'):
            return False
        if not self._local_access_allowed():
            self._send_json({'error': '无权访问本机定价设置'}, 403)
            return True
        try:
            if path == '/api/pricing' and self.command == 'GET':
                result = pricing.state()
            elif path == '/api/pricing/catalog' and self.command == 'GET':
                result = {'models': pricing.catalog()}
            elif path == '/api/pricing/settings' and self.command == 'PUT':
                if int(self.headers.get('Content-Length', '0')) > 4 * 1024 * 1024:
                    self._send_json({'error': '模型选择数据过大，请缩小选择范围'}, 413)
                    return True
                value = self._read_json(4 * 1024 * 1024)
                if not value:
                    raise ValueError('模型选择数据无效')
                result = pricing.update_settings(value)
            elif path == '/api/pricing/entry' and self.command == 'PUT':
                result = pricing.save_entry(self._read_json())
            elif path == '/api/pricing/entry' and self.command == 'DELETE':
                result = pricing.delete_entry(self._read_json().get('model_id'))
            elif path == '/api/pricing/sync' and self.command == 'POST':
                result = pricing.sync()
            else:
                self._send_json({'error': '接口不存在'}, 404)
                return True
            self._send_json(result)
        except (OSError, ValueError, TypeError, KeyError) as error:
            self._send_json({'error': str(error)}, 400)
        return True

    def do_GET(self) -> None:  # noqa: N802
        parsed = urlparse(self.path)
        if self._pricing_request(parsed.path):
            return
        if parsed.path == "/api/dashboard":
            self._send_dashboard()
            return
        if parsed.path == "/api/dashboard/status":
            self._send(json.dumps(_dashboard_status(), ensure_ascii=False, separators=(",", ":")).encode(), "application/json; charset=utf-8")
            return
        if parsed.path == "/api/usage":
            if not self._local_access_allowed():
                self._send_json({"error": "无权访问本机用量数据"}, 403)
                return
            if _usage_store is None:
                self._send_json({"error": "用量数据库尚未初始化"}, 503)
                return
            try:
                query = parse_qs(parsed.query)
                def number(name: str, default: int | None = None) -> int | None:
                    return int(query[name][0]) if query.get(name) else default
                filters = {name: query.get(name, ["all"])[0] for name in ("source", "model", "project")}
                filters.update({"query": query.get("query", [""])[0][:500], "hidden": query.get("hidden", []),
                                "start": number("start"), "end": number("end"), "now": number("now"),
                                "page": number("page", 1), "page_size": number("page_size", 20)})
                payload = _usage_store.query(filters, pricing.state()["prices"], include_board=query.get("include_board", ["1"])[0] != "0")
                self._send(json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode(), "application/json; charset=utf-8")
            except (ValueError, OverflowError) as error:
                self._send_json({"error": str(error)}, 400)
            return
        if parsed.path == "/api/source-settings":
            self._send(json.dumps(_load_settings(), ensure_ascii=False).encode(), "application/json; charset=utf-8")
            return
        if parsed.path == "/api/sync-settings":
            if not self._local_access_allowed():
                self._send_json({'error': '无权访问本机同步设置'}, 403)
                return
            self._send(json.dumps(_load_sync_settings(), ensure_ascii=False).encode(), "application/json; charset=utf-8")
            return
        if parsed.path == "/api/board-layout":
            self._send(json.dumps(_load_board_layout(), ensure_ascii=False, separators=(",", ":")).encode(), "application/json; charset=utf-8")
            return
        if parsed.path == "/api/board-style":
            if not self._local_access_allowed():
                self._send_json({"error": "无权访问看板样式"}, 403)
                return
            self._send_json(_load_board_style())
            return
        if parsed.path == "/api/board-settings":
            self._send(json.dumps(_load_board_settings(), ensure_ascii=False, separators=(",", ":")).encode(), "application/json; charset=utf-8")
            return
        if parsed.path == "/api/danmaku":
            self._send(json.dumps(_danmaku_snapshot(), ensure_ascii=False, separators=(",", ":")).encode(), "application/json; charset=utf-8")
            return
        if parsed.path == "/api/export":
            fmt = parse_qs(parsed.query).get("format", ["json"])[0]
            records = list(_usage_store.all_records()) if _usage_store is not None else _collect()[1]
            if fmt == "csv":
                output = io.StringIO()
                writer = csv.DictWriter(output, fieldnames=list(records[0]) if records else ["source", "session_id", "timestamp", "project", "model", "input_tokens", "output_tokens", "reasoning_tokens", "cache_read_tokens", "cache_write_tokens", "total_tokens", "cost"])
                writer.writeheader()
                writer.writerows(records)
                self._send(output.getvalue().encode("utf-8-sig"), "text/csv; charset=utf-8")
            else:
                self._send(json.dumps(records, ensure_ascii=False, indent=2).encode(), "application/json; charset=utf-8")
            return
        if parsed.path.startswith("/api/"):
            self._send_json({"error": "not found"}, 404)
            return
        if parsed.path == "/" or not Path(parsed.path.lstrip("/")).suffix:
            self.path = "/index.html"
        super().do_GET()

    def _read_json(self, max_bytes: int = 64 * 1024) -> dict:
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length < 0 or length > max_bytes:
                return {}
            return json.loads(self.rfile.read(length).decode("utf-8")) if length else {}
        except (ValueError, json.JSONDecodeError, UnicodeDecodeError):
            return {}

    def do_PUT(self) -> None:  # noqa: N802
        path = urlparse(self.path).path
        if self._pricing_request(path):
            return
        if path == "/api/board-layout":
            try:
                layout = _save_board_layout(self._read_json())
                self._send(json.dumps(layout, ensure_ascii=False, separators=(",", ":")).encode(), "application/json; charset=utf-8")
            except ValueError as error:
                self._send(json.dumps({"error": str(error)}, ensure_ascii=False).encode(), "application/json; charset=utf-8", 400)
            return
        if path == "/api/board-style":
            if not self._local_access_allowed():
                self._send_json({"error": "无权修改看板样式"}, 403)
                return
            try:
                style = _normalize_board_style(self._read_json())
                _atomic_write_text(BOARD_STYLE_PATH, json.dumps(style, ensure_ascii=False))
                self._send_json(style)
            except ValueError as error:
                self._send_json({"error": str(error)}, 400)
            except OSError:
                self._send_json({"error": "无法保存看板样式"}, 500)
            return
        if path == "/api/board-settings":
            try:
                settings = _save_board_settings(self._read_json())
                self._send(json.dumps(settings, ensure_ascii=False, separators=(",", ":")).encode(), "application/json; charset=utf-8")
            except ValueError as error:
                self._send(json.dumps({"error": str(error)}, ensure_ascii=False).encode(), "application/json; charset=utf-8", 400)
            return
        if path == "/api/danmaku/settings":
            if not self._local_access_allowed():
                self._send_json({"error": "无权修改弹幕设置"}, 403)
                return
            try:
                requested = self._read_json()
                room_id = int(requested.get("room_id", 0))
                if room_id < 0:
                    raise ValueError("直播间号不能为负数")
                cookie = requested.get("cookie")
                if cookie is not None and not isinstance(cookie, str):
                    raise ValueError("Cookie 必须是字符串")
                font_size = requested.get("font_size")
                if font_size is not None and not isinstance(font_size, (int, float)):
                    raise ValueError("弹幕字号必须是数字")
                payload = _configure_danmaku(room_id, cookie, bool(requested.get("clear_cookie", False)), int(font_size) if font_size is not None else None)
                self._send(json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode(), "application/json; charset=utf-8")
            except (OSError, TypeError, ValueError) as error:
                self._send(json.dumps({"error": str(error)}, ensure_ascii=False).encode(), "application/json; charset=utf-8", 400)
            return
        if path == "/api/sync-settings":
            if not self._local_access_allowed():
                self._send_json({'error': '无权访问本机同步设置'}, 403)
                return
            try:
                self._send(json.dumps(_update_sync_settings(self._read_json()), ensure_ascii=False).encode(), "application/json; charset=utf-8")
            except (ValueError, OSError) as error:
                self._send(json.dumps({"error": str(error)}, ensure_ascii=False).encode(), "application/json; charset=utf-8", 400)
            return
        if path != "/api/source-settings":
            self.send_error(404)
            return
        requested = self._read_json()
        settings = _update_source_settings(requested)
        self._send(json.dumps(settings, ensure_ascii=False).encode(), "application/json; charset=utf-8")

    def _danmaku_login_request(self, path: str) -> bool:
        prefix = "/api/danmaku/login/"
        if not path.startswith(prefix):
            return False
        if not self._local_access_allowed():
            self._send_json({"error": "无权修改 B 站登录状态"}, 403)
            return True
        action = path[len(prefix):]
        if action not in ("start", "poll", "cancel"):
            self._send_json({"error": "not found"}, 404)
            return True
        body = self._read_json()
        session_id = body.get("session_id") if isinstance(body, dict) else None
        try:
            if action == "start":
                result = _bilibili_qr.start()
            elif action == "poll":
                result = _bilibili_qr.poll(session_id, lambda cookie: _configure_danmaku(None, cookie))
            else:
                result = _bilibili_qr.cancel(session_id)
            self._send_json(result)
        except LoginUnavailable as error:
            self._send_json({"error": str(error)}, 502)
        except ValueError as error:
            self._send_json({"error": str(error)}, 400)
        except OSError:
            self._send_json({"error": "无法验证或保存 B 站登录，请稍后重试。"}, 502)
        return True

    def do_POST(self) -> None:  # noqa: N802
        path = urlparse(self.path).path
        if self._danmaku_login_request(path) or self._pricing_request(path):
            return
        if path == "/api/sync/heartbeat":
            if not self._local_access_allowed():
                self._send_json({'error': '无权访问本机同步设置'}, 403)
                return
            _mark_usage_foreground(self._read_json().get("opened") is True)
            self._send_json({'ok': True})
            return
        if path not in ("/api/scan", "/api/codex/rebuild"):
            self.send_error(404)
            return
        body = self._read_json()
        requested = body.get("sources", list(SOURCES))
        sources = [source for source in requested if source in SOURCES] if isinstance(requested, list) else []
        if path == "/api/codex/rebuild":
            sources = ["codex"]
        if not sources:
            self._send(json.dumps({"error": "请选择要同步的数据源"}, ensure_ascii=False).encode(), "application/json; charset=utf-8", 400)
            return
        if not _scan_request_lock.acquire(blocking=False):
            self._send(json.dumps({"error": "同步正在进行中，请稍后重试"}, ensure_ascii=False).encode(), "application/json; charset=utf-8", 409)
            return
        try:
            payload, _ = _collect_fresh(sources, force=bool(body.get("force", True)), rebuild_codex=path == "/api/codex/rebuild")
            compact = {**_dashboard_status(payload), "scanned_sources": sources}
            self._send(json.dumps(compact, ensure_ascii=False, separators=(",", ":")).encode(), "application/json; charset=utf-8")
        except Exception as error:
            self._send(json.dumps({"error": str(error)}, ensure_ascii=False).encode(), "application/json; charset=utf-8", 500)
        finally:
            _scan_request_lock.release()

    def do_DELETE(self) -> None:  # noqa: N802
        if not self._pricing_request(urlparse(self.path).path):
            self.send_error(404)


class LocalWebServer(ThreadingHTTPServer):
    if os.name == "nt":
        allow_reuse_address = False

    def server_bind(self) -> None:
        # On Windows SO_REUSEADDR can let a second launch bind the same port.
        if os.name == "nt":
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        super().server_bind()


def main() -> None:
    global _usage_store
    if not DIST.exists():
        raise SystemExit("未找到 dist，请先运行 npm run build")
    if os.environ.get("TOKENUSAGE_DATA_DIR"):
        pricing.PATH = DATA_ROOT / "cache" / "model-pricing.json"
    cache = DATA_ROOT / "cache"
    _usage_store = UsageStore(cache / "usage.sqlite", DATA_ROOT / "backups")
    _usage_store.initialize(cache / "dashboard.json", cache / "codex-rollouts.json", cache / "claude-sessions.json")
    server = LocalWebServer(("127.0.0.1", PORT), Handler)
    threading.Thread(target=_usage_backup_loop, daemon=True, name="tokenusage-usage-backup").start()
    threading.Thread(target=_scan_enabled_sources, daemon=True, name="tokenusage-startup-sync").start()
    threading.Thread(target=_scheduler_loop, daemon=True, name="tokenusage-source-scheduler").start()
    threading.Thread(target=_pricing_scheduler_loop, daemon=True, name="tokenusage-pricing-scheduler").start()
    print(f"TokenUsage 网页端: http://127.0.0.1:{PORT}")
    try:
        server.serve_forever()
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
