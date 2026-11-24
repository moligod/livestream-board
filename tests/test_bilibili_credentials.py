from __future__ import annotations

import unittest
import os

from tokenusage.bilibili_credentials import _protect, _unprotect, cookie_identity, parse_cookie, sanitize_cookie


class BilibiliCredentialTests(unittest.TestCase):
    def test_cookie_is_reduced_to_login_allowlist(self) -> None:
        cookie = sanitize_cookie("foo=secret; DedeUserID=123; SESSDATA=session; bili_jct=csrf; buvid3=device")
        self.assertNotIn("foo", cookie)
        self.assertEqual(parse_cookie(cookie)["SESSDATA"], "session")
        self.assertEqual(cookie_identity(cookie), (123, "device"))

    def test_cookie_requires_login_identity(self) -> None:
        with self.assertRaisesRegex(ValueError, "SESSDATA"):
            sanitize_cookie("bili_jct=csrf")

    @unittest.skipUnless(os.name == "nt", "DPAPI is Windows-only")
    def test_dpapi_round_trip_is_user_bound_and_lossless(self) -> None:
        plaintext = b"SESSDATA=session; DedeUserID=123"
        encrypted = _protect(plaintext)
        self.assertNotIn(b"session", encrypted)
        self.assertEqual(_unprotect(encrypted), plaintext)


if __name__ == "__main__":
    unittest.main()
