from __future__ import annotations

import json
import hashlib
import struct
import threading
import time
import urllib.parse
import urllib.request
import uuid
import zlib
from collections import deque
from typing import Any, Iterable

import brotli
import websocket

from .bilibili_credentials import cookie_identity

HEADER = struct.Struct(">IHHII")
OP_HEARTBEAT = 2
OP_MESSAGE = 5
OP_AUTH = 7
USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36"
MIXIN_KEY_ENC_TAB = (
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35,
    27, 43, 5, 49, 33, 9, 42, 19, 29, 28, 14, 39, 12, 38, 41, 13,
    37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4,
    22, 25, 54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
)
_wbi_lock = threading.Lock()
_wbi_cache: tuple[str, float] | None = None


def pack_packet(body: bytes, operation: int, version: int = 1, sequence: int = 1) -> bytes:
    return HEADER.pack(HEADER.size + len(body), HEADER.size, version, operation, sequence) + body


def iter_json_messages(data: bytes) -> Iterable[dict[str, Any]]:
    offset = 0
    while offset + HEADER.size <= len(data):
        packet_length, header_length, version, operation, _sequence = HEADER.unpack_from(data, offset)
        if packet_length < header_length or offset + packet_length > len(data):
            return
        body = data[offset + header_length : offset + packet_length]
        offset += packet_length
        if version == 2:
            try:
                yield from iter_json_messages(zlib.decompress(body))
            except zlib.error:
                continue
        elif version == 3:
            try:
                yield from iter_json_messages(brotli.decompress(body))
            except brotli.error:
                continue
        elif operation == OP_MESSAGE:
            try:
                value = json.loads(body.decode("utf-8").rstrip("\x00"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
            if isinstance(value, dict):
                yield value


def parse_danmaku_message(value: dict[str, Any]) -> dict[str, Any] | None:
    command = str(value.get("cmd", ""))
    if not command.startswith("DANMU_MSG"):
        return None
    info = value.get("info")
    if not isinstance(info, list) or len(info) < 3:
        return None
    user = info[2] if isinstance(info[2], list) else []
    metadata = info[0] if isinstance(info[0], list) else []
    medal = info[3] if len(info) > 3 and isinstance(info[3], list) else []
    user_level_info = info[4] if len(info) > 4 and isinstance(info[4], list) else []
    try:
        text = str(info[1])
        username = str(user[1]) if len(user) > 1 else "匿名用户"
        uid = int(user[0]) if user else 0
    except (TypeError, ValueError):
        return None
    raw_timestamp = metadata[4] if len(metadata) > 4 and isinstance(metadata[4], (int, float)) else int(time.time() * 1000)
    timestamp = int(raw_timestamp / 1000) if raw_timestamp > 10_000_000_000 else int(raw_timestamp)
    message_id = str(value.get("msg_id") or value.get("id_str") or f"{uid}:{timestamp}:{text}")
    return {
        "id": message_id,
        "uid": uid,
        "user": username,
        "text": text,
        "timestamp": timestamp,
        "medal": str(medal[1]) if len(medal) > 1 else "",
        "medal_level": int(medal[0]) if medal and isinstance(medal[0], (int, float)) else 0,
        "user_level": int(user_level_info[0]) if user_level_info and isinstance(user_level_info[0], (int, float)) else 0,
    }


def _request_json(url: str, params: dict[str, Any] | None = None, cookie: str = "") -> dict[str, Any]:
    query = urllib.parse.urlencode(params or {})
    headers = {"User-Agent": USER_AGENT, "Referer": "https://www.bilibili.com/"}
    if cookie:
        headers["Cookie"] = cookie
    request = urllib.request.Request(
        f"{url}?{query}" if query else url,
        headers=headers,
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        raw = response.read(2 * 1024 * 1024)
    payload = json.loads(raw)
    if not isinstance(payload, dict):
        raise RuntimeError("B站接口返回格式异常")
    return payload


def _get_json(url: str, params: dict[str, Any], cookie: str = "") -> dict[str, Any]:
    payload = _request_json(url, params, cookie)
    if payload.get("code") != 0 or not isinstance(payload.get("data"), dict):
        message = str(payload.get("message") or "B站接口返回异常")
        raise RuntimeError(f"{message}（code {payload.get('code', 'unknown')}）")
    return payload["data"]


def _get_wbi_mixin_key(*, now: float | None = None) -> str:
    global _wbi_cache
    current = time.time() if now is None else now
    with _wbi_lock:
        if _wbi_cache and current - _wbi_cache[1] < 30 * 60:
            return _wbi_cache[0]
        payload = _request_json("https://api.bilibili.com/x/web-interface/nav")
        data = payload.get("data")
        wbi_img = data.get("wbi_img") if isinstance(data, dict) else None
        if not isinstance(wbi_img, dict):
            raise RuntimeError("无法获取 B 站 WBI 密钥")
        img_key = str(wbi_img.get("img_url") or "").rsplit("/", 1)[-1].split(".", 1)[0]
        sub_key = str(wbi_img.get("sub_url") or "").rsplit("/", 1)[-1].split(".", 1)[0]
        source = img_key + sub_key
        if len(source) < 64:
            raise RuntimeError("B 站 WBI 密钥格式异常")
        mixin_key = "".join(source[index] for index in MIXIN_KEY_ENC_TAB)[:32]
        _wbi_cache = (mixin_key, current)
        return mixin_key


def sign_wbi_params(params: dict[str, Any], mixin_key: str, *, timestamp: int | None = None) -> dict[str, Any]:
    signed = {**params, "wts": int(time.time()) if timestamp is None else int(timestamp)}
    cleaned = {
        key: "".join(character for character in str(value) if character not in "!'()*")
        for key, value in signed.items()
    }
    query = urllib.parse.urlencode(sorted(cleaned.items()))
    signed["w_rid"] = hashlib.md5(f"{query}{mixin_key}".encode()).hexdigest()
    return signed


def validate_login_cookie(cookie: str) -> tuple[int, str]:
    uid, buvid = cookie_identity(cookie)
    payload = _request_json("https://api.bilibili.com/x/web-interface/nav", cookie=cookie)
    data = payload.get("data")
    if payload.get("code") != 0 or not isinstance(data, dict) or not data.get("isLogin"):
        raise ValueError("B站 Cookie 已失效或未登录")
    account_uid = int(data.get("mid") or 0)
    if account_uid < 1 or (uid and account_uid != uid):
        raise ValueError("B站 Cookie 用户身份不一致")
    return account_uid, buvid


def resolve_room(room_id: int, cookie: str = "") -> int:
    data = _get_json("https://api.live.bilibili.com/room/v1/Room/room_init", {"id": room_id}, cookie)
    resolved = int(data.get("room_id") or 0)
    if resolved < 1:
        raise RuntimeError("直播间不存在")
    return resolved


def get_connection_info(room_id: int, cookie: str = "") -> tuple[str, str]:
    params = sign_wbi_params(
        {"id": room_id, "type": 0, "web_location": "444.8"},
        _get_wbi_mixin_key(),
    )
    data = _get_json("https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo", params, cookie)
    token = str(data.get("token") or "")
    hosts = data.get("host_list")
    if not token or not isinstance(hosts, list) or not hosts:
        raise RuntimeError("无法获取弹幕服务器")
    host = hosts[0]
    return f"wss://{host['host']}:{int(host['wss_port'])}/sub", token


class BilibiliDanmakuService:
    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._socket: websocket.WebSocket | None = None
        self._room_id = 0
        self._cookie = ""
        self._real_room_id = 0
        self._status = "not-configured"
        self._error: str | None = None
        self._revision = 0
        self._messages: deque[dict[str, Any]] = deque(maxlen=50)

    def configure(self, room_id: int, cookie: str = "") -> None:
        room_id = max(0, int(room_id))
        with self._lock:
            if room_id == self._room_id and cookie == self._cookie and self._thread and self._thread.is_alive():
                return
            self._room_id = room_id
            self._cookie = cookie
            self._real_room_id = 0
            self._error = None
            self._messages.clear()
            self._revision += 1
            self._stop.set()
            if self._socket:
                try:
                    self._socket.close()
                except OSError:
                    pass
            self._stop = threading.Event()
            if room_id < 1:
                self._status = "not-configured"
                self._thread = None
                return
            self._status = "connecting"
            self._thread = threading.Thread(target=self._run, args=(room_id, self._stop), daemon=True, name="bilibili-danmaku")
            self._thread.start()

    def snapshot(self) -> dict[str, Any]:
        with self._lock:
            return {
                "room_id": self._room_id,
                "real_room_id": self._real_room_id,
                "status": self._status,
                "authenticated": bool(self._cookie),
                "error": self._error,
                "revision": self._revision,
                "messages": list(self._messages),
            }

    def _set_state(self, status: str, error: str | None = None, real_room_id: int | None = None, *, expected_room_id: int | None = None, expected_stop: threading.Event | None = None) -> bool:
        with self._lock:
            if expected_room_id is not None and self._room_id != expected_room_id:
                return False
            if expected_stop is not None and self._stop is not expected_stop:
                return False
            self._status = status
            self._error = error
            if real_room_id is not None:
                self._real_room_id = real_room_id
            self._revision += 1
            return True

    def _append(self, message: dict[str, Any], room_id: int, stop: threading.Event) -> None:
        with self._lock:
            if self._room_id != room_id or self._stop is not stop:
                return
            if any(existing["id"] == message["id"] for existing in self._messages):
                return
            self._messages.append(message)
            self._revision += 1

    def _run(self, room_id: int, stop: threading.Event) -> None:
        backoff = 1.0
        while not stop.is_set():
            socket: websocket.WebSocket | None = None
            try:
                cookie = self._cookie
                uid, buvid = cookie_identity(cookie)
                real_room_id = resolve_room(room_id, cookie)
                url, token = get_connection_info(real_room_id, cookie)
                if not self._set_state("connecting", real_room_id=real_room_id, expected_room_id=room_id, expected_stop=stop):
                    return
                socket_headers = [f"User-Agent: {USER_AGENT}"]
                if cookie:
                    socket_headers.append(f"Cookie: {cookie}")
                socket = websocket.create_connection(url, timeout=10, origin="https://live.bilibili.com", header=socket_headers)
                with self._lock:
                    if self._room_id != room_id or self._stop is not stop:
                        socket.close()
                        return
                    self._socket = socket
                auth = json.dumps({
                    "uid": uid,
                    "roomid": real_room_id,
                    "protover": 3,
                    "buvid": buvid or f"{uuid.uuid4()}infoc",
                    "platform": "web",
                    "type": 2,
                    "key": token,
                    "support_ack": True,
                    "queue_uuid": "",
                    "scene": "room",
                }, separators=(",", ":")).encode()
                socket.send_binary(pack_packet(auth, OP_AUTH))
                socket.settimeout(1)
                self._set_state("connected", real_room_id=real_room_id, expected_room_id=room_id, expected_stop=stop)
                backoff = 1.0
                next_heartbeat = 0.0
                while not stop.is_set():
                    if time.monotonic() >= next_heartbeat:
                        socket.send_binary(pack_packet(b"[object Object]", OP_HEARTBEAT))
                        next_heartbeat = time.monotonic() + 30
                    try:
                        payload = socket.recv()
                    except websocket.WebSocketTimeoutException:
                        continue
                    if isinstance(payload, str):
                        payload = payload.encode()
                    if not isinstance(payload, (bytes, bytearray)):
                        continue
                    for item in iter_json_messages(bytes(payload)):
                        message = parse_danmaku_message(item)
                        if message:
                            self._append(message, room_id, stop)
            except Exception as error:
                if not stop.is_set():
                    self._set_state("reconnecting", str(error), expected_room_id=room_id, expected_stop=stop)
            finally:
                if socket:
                    try:
                        socket.close()
                    except OSError:
                        pass
                with self._lock:
                    if self._socket is socket:
                        self._socket = None
            if stop.wait(backoff):
                break
            backoff = min(30.0, backoff * 2)
        if stop.is_set():
            self._set_state("disconnected", expected_room_id=room_id, expected_stop=stop)
