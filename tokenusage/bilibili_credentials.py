from __future__ import annotations

import ctypes
import os
from ctypes import wintypes
from pathlib import Path

ALLOWED_COOKIE_KEYS = (
    "SESSDATA",
    "bili_jct",
    "DedeUserID",
    "DedeUserID__ckMd5",
    "buvid3",
    "buvid4",
    "buvid_fp",
    "sid",
)
COOKIE_PATH = Path(os.environ.get("LOCALAPPDATA", Path.home() / "AppData" / "Local")) / "TokenUsage" / "credentials" / "bilibili-cookie.dpapi"
_ENTROPY = b"TokenUsage:BilibiliCookie:v1"


class _DataBlob(ctypes.Structure):
    _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_ubyte))]


def parse_cookie(raw: str) -> dict[str, str]:
    values: dict[str, str] = {}
    allowed = set(ALLOWED_COOKIE_KEYS)
    for part in str(raw or "").split(";"):
        key, separator, value = part.strip().partition("=")
        if separator and key in allowed and value:
            values[key] = value.strip()
    return values


def sanitize_cookie(raw: str) -> str:
    values = parse_cookie(raw)
    if not values.get("SESSDATA") or not values.get("DedeUserID"):
        raise ValueError("B站 Cookie 至少需要 SESSDATA 和 DedeUserID")
    return "; ".join(f"{key}={values[key]}" for key in ALLOWED_COOKIE_KEYS if key in values)


def cookie_identity(cookie: str) -> tuple[int, str]:
    values = parse_cookie(cookie)
    try:
        uid = max(0, int(values.get("DedeUserID", "0")))
    except ValueError:
        uid = 0
    return uid, values.get("buvid3") or values.get("buvid4") or ""


def _blob(data: bytes) -> tuple[_DataBlob, ctypes.Array[ctypes.c_char]]:
    buffer = ctypes.create_string_buffer(data)
    return _DataBlob(len(data), ctypes.cast(buffer, ctypes.POINTER(ctypes.c_ubyte))), buffer


def _protect(data: bytes) -> bytes:
    if os.name != "nt":
        raise RuntimeError("B站 Cookie 加密存储目前仅支持 Windows")
    crypt32 = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    source, source_buffer = _blob(data)
    entropy, entropy_buffer = _blob(_ENTROPY)
    output = _DataBlob()
    result = crypt32.CryptProtectData(
        ctypes.byref(source),
        "TokenUsage Bilibili Cookie",
        ctypes.byref(entropy),
        None,
        None,
        0x1,
        ctypes.byref(output),
    )
    _ = source_buffer, entropy_buffer
    if not result:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        return ctypes.string_at(output.pbData, output.cbData)
    finally:
        kernel32.LocalFree(output.pbData)


def _unprotect(data: bytes) -> bytes:
    if os.name != "nt":
        raise RuntimeError("B站 Cookie 解密存储目前仅支持 Windows")
    crypt32 = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    source, source_buffer = _blob(data)
    entropy, entropy_buffer = _blob(_ENTROPY)
    output = _DataBlob()
    result = crypt32.CryptUnprotectData(
        ctypes.byref(source),
        None,
        ctypes.byref(entropy),
        None,
        None,
        0x1,
        ctypes.byref(output),
    )
    _ = source_buffer, entropy_buffer
    if not result:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        return ctypes.string_at(output.pbData, output.cbData)
    finally:
        kernel32.LocalFree(output.pbData)


def save_bilibili_cookie(raw: str) -> str:
    cookie = sanitize_cookie(raw)
    encrypted = _protect(cookie.encode("utf-8"))
    COOKIE_PATH.parent.mkdir(parents=True, exist_ok=True)
    temporary = COOKIE_PATH.with_suffix(f".tmp.{os.getpid()}")
    temporary.write_bytes(encrypted)
    os.replace(temporary, COOKIE_PATH)
    return cookie


def load_bilibili_cookie() -> str:
    try:
        return _unprotect(COOKIE_PATH.read_bytes()).decode("utf-8")
    except (OSError, UnicodeDecodeError, RuntimeError):
        return ""


def clear_bilibili_cookie() -> None:
    COOKIE_PATH.unlink(missing_ok=True)
