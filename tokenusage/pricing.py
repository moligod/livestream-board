"""Local model prices and opt-in models.dev synchronization.

Prices are estimates per million tokens. The catalog request contains no local
usage data, and manually edited prices are never replaced by a later sync.
"""
from __future__ import annotations

import json
import math
import os
import threading
import time
from pathlib import Path
from urllib.request import Request, urlopen

PATH = Path(__file__).resolve().parents[1] / "data" / "cache" / "model-pricing.json"
CATALOG_URL = "https://models.dev/api.json"
SYNC_INTERVAL = 6 * 60 * 60
_lock = threading.RLock()
_sync_lock = threading.Lock()
_catalog_lock = threading.Lock()
_catalog_cache: list[dict] | None = None
_catalog_at = 0.0


def _default() -> dict:
    return {"version": 1, "auto_sync": False, "selected": [], "last_synced_at": 0, "last_error": None, "prices": {}}


def _load_unlocked() -> dict:
    try:
        value = json.loads(PATH.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return _default()
    if not isinstance(value, dict) or not isinstance(value.get("prices", {}), dict) or not isinstance(value.get("selected", []), list):
        raise ValueError("模型定价文件格式无效，请检查本地定价文件")
    return {**_default(), **value}


def _save_unlocked(value: dict) -> None:
    PATH.parent.mkdir(parents=True, exist_ok=True)
    temporary = PATH.with_name(f".{PATH.name}.{os.getpid()}.{threading.get_ident()}.tmp")
    try:
        temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")
        os.replace(temporary, PATH)
    finally:
        temporary.unlink(missing_ok=True)


def state() -> dict:
    with _lock:
        value = _load_unlocked()
        return {**value, "prices": sorted(value["prices"].values(), key=lambda item: item["model_id"].lower())}


def update_settings(value: dict) -> dict:
    with _lock:
        current = _load_unlocked()
        if "selected" in value:
            selected = value["selected"]
            if not isinstance(selected, list) or len(selected) > 20000 or any(not isinstance(item, str) or not item or len(item) > 300 for item in selected):
                raise ValueError("模型选择无效，最多支持 20000 个模型")
            current["selected"] = list(dict.fromkeys(selected))
        if "auto_sync" in value:
            if not isinstance(value["auto_sync"], bool):
                raise ValueError("自动同步设置无效")
            if value["auto_sync"] and not current["selected"]:
                raise ValueError("请先选择要同步的模型")
            current["auto_sync"] = value["auto_sync"]
        if not current["selected"]:
            current["auto_sync"] = False
        _save_unlocked(current)
        return state()


def _rate(value: object, *, optional: bool = False) -> float | None:
    if value is None and optional:
        return None
    if isinstance(value, bool):
        raise ValueError("价格必须是非负数字")
    try:
        number = float(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        raise ValueError("价格必须是非负数字") from None
    if not math.isfinite(number) or number < 0 or number > 1_000_000:
        raise ValueError("价格必须是非负数字")
    return number


def _entry(value: dict, origin: str) -> dict:
    model_id = value.get("model_id")
    name = value.get("name", model_id)
    provider = value.get("provider", "")
    if not isinstance(model_id, str) or not model_id.strip() or len(model_id) > 200:
        raise ValueError("请输入有效的模型 ID")
    if not isinstance(name, str) or len(name) > 200 or not isinstance(provider, str) or len(provider) > 100:
        raise ValueError("模型名称或供应商无效")
    return {"model_id": model_id.strip(), "name": name.strip() or model_id.strip(), "provider": provider.strip(),
            "input": _rate(value.get("input")), "output": _rate(value.get("output")),
            "cache_read": _rate(value.get("cache_read"), optional=True),
            "cache_write": _rate(value.get("cache_write"), optional=True), "origin": origin}


def save_entry(value: dict) -> dict:
    entry = _entry(value, "manual")
    with _lock:
        current = _load_unlocked()
        current["prices"][entry["model_id"].lower()] = entry
        _save_unlocked(current)
        return state()


def delete_entry(model_id: object) -> dict:
    if not isinstance(model_id, str) or not model_id or len(model_id) > 200:
        raise ValueError("模型 ID 无效")
    with _lock:
        current = _load_unlocked()
        current["prices"].pop(model_id.lower(), None)
        current["selected"] = [key for key in current["selected"] if not key.lower().endswith("/" + model_id.lower())]
        if not current["selected"]:
            current["auto_sync"] = False
        _save_unlocked(current)
        return state()


def catalog(force: bool = False) -> list[dict]:
    global _catalog_cache, _catalog_at
    with _catalog_lock:
        if not force and _catalog_cache is not None and time.monotonic() - _catalog_at < SYNC_INTERVAL:
            return _catalog_cache
        request = Request(CATALOG_URL, headers={"User-Agent": "TokenUsage/1.0", "Accept": "application/json"})
        with urlopen(request, timeout=20) as response:
            data = response.read(16 * 1024 * 1024 + 1)
        if len(data) > 16 * 1024 * 1024:
            raise ValueError("models.dev 数据过大")
        raw = json.loads(data)
        if not isinstance(raw, dict):
            raise ValueError("models.dev 数据格式无效")
        result = []
        for provider_id, provider in raw.items():
            if not isinstance(provider_id, str) or not isinstance(provider, dict) or not isinstance(provider.get("models"), dict):
                continue
            for model_id, model in provider["models"].items():
                if not isinstance(model_id, str) or not isinstance(model, dict) or not isinstance(model.get("cost"), dict):
                    continue
                cost = model["cost"]
                try:
                    entry = _entry({"model_id": model_id, "name": model.get("name") or model_id, "provider": provider_id,
                                    "input": cost.get("input"), "output": cost.get("output"),
                                    "cache_read": cost.get("cache_read"), "cache_write": cost.get("cache_write")}, "models.dev")
                except ValueError:
                    continue
                entry["key"] = f"{provider_id}/{model_id}"
                result.append(entry)
        if not result:
            raise ValueError("models.dev 未返回可用的模型价格")
        _catalog_cache, _catalog_at = sorted(result, key=lambda row: (row["provider"], row["model_id"])), time.monotonic()
        return _catalog_cache


def sync(*, automatic: bool = False) -> dict:
    if not _sync_lock.acquire(blocking=False):
        raise ValueError("模型定价同步正在进行")
    try:
        with _lock:
            before = _load_unlocked()
            if automatic and (not before["auto_sync"] or time.time() - before["last_synced_at"] < SYNC_INTERVAL):
                return {"skipped": True, "updated": 0, "preserved": 0, "state": state()}
            if not before["selected"]:
                raise ValueError("请先选择要同步的模型")
        try:
            entries = {item["key"]: item for item in catalog(force=True)}
            with _lock:
                current = _load_unlocked()
                if automatic and not current["auto_sync"]:
                    return {"skipped": True, "updated": 0, "preserved": 0, "state": state()}
                missing = [key for key in current["selected"] if key not in entries]
                if current["selected"] and len(missing) == len(current["selected"]):
                    raise ValueError("已选模型在 models.dev 中均未找到，请重新选择")
                updated = preserved = 0
                seen_models = set()
                for key in current["selected"]:
                    item = entries.get(key)
                    if item is None:
                        continue
                    model_key = item["model_id"].lower()
                    if model_key in seen_models:
                        raise ValueError(f"模型 {item['model_id']} 选择了多个供应商，请只保留一个")
                    seen_models.add(model_key)
                    existing = current["prices"].get(model_key)
                    if existing and existing.get("origin") == "manual":
                        preserved += 1
                        continue
                    current["prices"][model_key] = {field: value for field, value in item.items() if field != "key"}
                    updated += 1
                current["last_synced_at"] = int(time.time())
                current["last_error"] = f"{len(missing)} 个已选模型未在 models.dev 找到" if missing else None
                _save_unlocked(current)
                return {"skipped": False, "updated": updated, "preserved": preserved, "state": state()}
        except Exception as error:
            with _lock:
                current = _load_unlocked()
                current["last_error"] = str(error)[:500]
                _save_unlocked(current)
            raise
    finally:
        _sync_lock.release()
