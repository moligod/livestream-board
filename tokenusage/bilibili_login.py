from __future__ import annotations

import base64
import json
import secrets
import threading
import time
from dataclasses import dataclass
from http.cookies import CookieError, SimpleCookie
from urllib.parse import urlencode, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

from .bilibili_credentials import ALLOWED_COOKIE_KEYS, sanitize_cookie
from .login_qr import svg


class LoginUnavailable(RuntimeError):
    """An upstream or local login dependency failed."""


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_passport_opener = build_opener(_NoRedirect())
_PASSPORT = "https://passport.bilibili.com/x/passport-login/web/qrcode/"
_TTL_SECONDS = 180
_POLL_SECONDS = 1.5


def _passport(action: str, key: str = "") -> tuple[dict, list[str]]:
    url = _PASSPORT + action + (("?" + urlencode({"qrcode_key": key})) if key else "")
    request = Request(url, headers={
        "Accept": "application/json",
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36",
        "Referer": "https://www.bilibili.com/",
    })
    try:
        with _passport_opener.open(request, timeout=8) as response:
            payload = json.loads(response.read(64 * 1024))
            cookies = response.headers.get_all("Set-Cookie", [])
    except (OSError, ValueError) as error:
        raise LoginUnavailable("暂时无法连接 B 站登录服务，请稍后重试。") from error
    data = payload.get("data") if isinstance(payload, dict) and payload.get("code") == 0 else None
    if not isinstance(data, dict):
        raise LoginUnavailable("B 站未接受扫码请求，可以稍后重试或使用网页登录。")
    return data, cookies


def _allowed_cookie(values: dict) -> str:
    return sanitize_cookie("; ".join(
        f"{name}={values[name]}" for name in ALLOWED_COOKIE_KEYS
        if isinstance(values.get(name), str) and all(c not in values[name] for c in ";\r\n")
    ))


def _qr_image(url: str) -> str:
    try:
        image = svg(url)
    except ValueError as error:
        raise LoginUnavailable("B 站返回的二维码内容过长或无效，请重新扫码。") from error
    return "data:image/svg+xml;base64," + base64.b64encode(image).decode("ascii")


@dataclass
class _Session:
    id: str
    key: str
    expires: float
    next_poll: float = 0
    in_flight: bool = False
    status: str = "waiting"


class BilibiliQrLogin:
    def __init__(self) -> None:
        self._session: _Session | None = None
        self._lock = threading.Lock()

    def start(self) -> dict:
        data, _ = _passport("generate")
        key, url = data.get("qrcode_key"), data.get("url")
        parsed = urlsplit(url) if isinstance(url, str) else None
        official_qr_paths = {
            ("passport.bilibili.com", "/h5-app/passport/login/scan"),
            ("account.bilibili.com", "/h5/account-h5/auth/scan-web"),
        }
        if not isinstance(key, str) or not key.isalnum() or not 1 <= len(key) <= 64 or not parsed or parsed.scheme != "https" or (parsed.netloc, parsed.path) not in official_qr_paths:
            raise LoginUnavailable("B 站返回了无效的登录二维码。")
        image = _qr_image(url)
        session = _Session(secrets.token_urlsafe(24), key, time.monotonic() + _TTL_SECONDS)
        with self._lock:
            self._session = session
        return {"session_id": session.id, "image": image}

    def cancel(self, identifier: object) -> dict:
        with self._lock:
            if self._session and self._session.id == identifier:
                self._session = None
        return {"status": "cancelled"}

    def poll(self, identifier: object, commit) -> dict:
        with self._lock:
            session = self._session
            now = time.monotonic()
            if not session or session.id != identifier or now >= session.expires:
                return {"status": "expired"}
            if session.in_flight or now < session.next_poll:
                return {"status": session.status}
            session.in_flight = True
            session.next_poll = now + _POLL_SECONDS
        try:
            data, headers = _passport("poll", session.key)
        finally:
            with self._lock:
                session.in_flight = False
        with self._lock:
            if self._session is not session or time.monotonic() >= session.expires:
                return {"status": "expired"}
            code = data.get("code")
            if code in (86101, 86090):
                session.status = "scanned" if code == 86090 else "waiting"
                return {"status": session.status}
            if code in (86038, 86039):
                self._session = None
                return {"status": "expired"}
            if code != 0:
                raise LoginUnavailable("B 站扫码状态异常，请重新生成二维码。")
            self._session = None
            values = SimpleCookie()
            try:
                for header in headers:
                    values.load(header)
            except CookieError as error:
                raise LoginUnavailable("B 站返回的登录状态无效，请重新扫码。") from error
            cookie = _allowed_cookie({name: item.value for name, item in values.items()})
            return {"status": "authorized", "danmaku": commit(cookie)}
