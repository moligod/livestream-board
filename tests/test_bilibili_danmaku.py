from __future__ import annotations

import json
import struct
import threading
import unittest
import zlib

import brotli

from tokenusage.bilibili_danmaku import BilibiliDanmakuService, OP_MESSAGE, iter_json_messages, pack_packet, parse_danmaku_message, sign_wbi_params


class BilibiliDanmakuTests(unittest.TestCase):
    def setUp(self) -> None:
        self.message = {
            "cmd": "DANMU_MSG:4:0:2:2:2:0",
            "info": [
                [0, 1, 25, 16777215, 1715735563613],
                "测试弹幕",
                [103031126, "测试用户", 1],
                [12, "测试勋章", "主播", 123],
                [27, 0, 0, 0],
            ],
            "msg_id": "message-1",
        }

    def test_parse_danmaku_message_extracts_user_text_and_medal(self) -> None:
        parsed = parse_danmaku_message(self.message)
        self.assertEqual(parsed["user"], "测试用户")
        self.assertEqual(parsed["text"], "测试弹幕")
        self.assertEqual(parsed["medal"], "测试勋章")
        self.assertEqual(parsed["medal_level"], 12)
        self.assertEqual(parsed["user_level"], 27)

    def test_packet_decoder_supports_plain_zlib_and_brotli(self) -> None:
        plain = pack_packet(json.dumps(self.message, ensure_ascii=False).encode(), OP_MESSAGE, version=0)
        zlib_packet = pack_packet(zlib.compress(plain), OP_MESSAGE, version=2)
        brotli_packet = pack_packet(brotli.compress(plain), OP_MESSAGE, version=3)
        self.assertEqual(list(iter_json_messages(plain))[0]["msg_id"], "message-1")
        self.assertEqual(list(iter_json_messages(zlib_packet))[0]["msg_id"], "message-1")
        self.assertEqual(list(iter_json_messages(brotli_packet))[0]["msg_id"], "message-1")

    def test_non_danmaku_command_is_ignored(self) -> None:
        self.assertIsNone(parse_danmaku_message({"cmd": "SEND_GIFT"}))

    def test_wbi_signing_is_stable_and_filters_reserved_characters(self) -> None:
        signed = sign_wbi_params(
            {"id": 26445, "type": 0, "web_location": "444.8", "unsafe": "a!b(c)*d'e"},
            "ea1db124af3c7062474693fa704f4ff8",
            timestamp=1700000000,
        )
        self.assertEqual(signed["wts"], 1700000000)
        self.assertEqual(signed["w_rid"], "5a366c5a02c10b8e31b54f96a9bfaa29")

    def test_stale_connection_cannot_overwrite_new_room_state(self) -> None:
        service = BilibiliDanmakuService()
        service._room_id = 2
        current_stop = service._stop
        changed = service._set_state("disconnected", expected_room_id=1, expected_stop=threading.Event())
        self.assertFalse(changed)
        self.assertEqual(service.snapshot()["status"], "not-configured")
        self.assertTrue(service._set_state("connected", expected_room_id=2, expected_stop=current_stop))


if __name__ == "__main__":
    unittest.main()
