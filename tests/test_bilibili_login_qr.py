import base64
import unittest
import xml.etree.ElementTree as ET
from unittest.mock import patch

from tokenusage.bilibili_login import BilibiliQrLogin, LoginUnavailable
from tokenusage.login_qr import svg


class BilibiliLoginQrTests(unittest.TestCase):
    def test_serves_local_svg_and_keeps_upstream_url_validation(self):
        url = 'https://account.bilibili.com/h5/account-h5/auth/scan-web?fixture=not-a-real-session'
        with patch('tokenusage.bilibili_login._passport', return_value=({'qrcode_key': 'fixture123', 'url': url}, [])):
            reply = BilibiliQrLogin().start()
        self.assertTrue(reply['session_id'])
        prefix, image = reply['image'].split(',', 1)
        self.assertEqual(prefix, 'data:image/svg+xml;base64')
        self.assertEqual(base64.b64decode(image, validate=True), svg(url))
        tree = ET.fromstring(svg(url))
        self.assertEqual(tree.tag, '{http://www.w3.org/2000/svg}svg')
        self.assertEqual(tree[0].get('fill'), '#fff')
        self.assertEqual(tree[1].get('fill'), '#000')
        self.assertNotIn(url.encode(), svg(url))
        with patch('tokenusage.bilibili_login._passport', return_value=({'qrcode_key': 'fixture123', 'url': url.replace('account.bilibili.com', 'example.invalid')}, [])):
            with self.assertRaisesRegex(LoginUnavailable, '无效'):
                BilibiliQrLogin().start()

    def test_symbol_sizes_and_capacity(self):
        for count, size in [(62, 41), (63, 49), (106, 49), (107, 57), (152, 57), (153, 65),
                            (213, 65), (214, 73), (287, 73), (288, 81), (362, 81)]:
            with self.subTest(count=count):
                self.assertEqual(ET.fromstring(svg('a' * count)).get('viewBox'), f'0 0 {size} {size}')
        with self.assertRaisesRegex(ValueError, 'capacity'):
            svg('a' * 363)

    def test_out_of_range_upstream_value_is_reported_as_login_error(self):
        url = 'https://passport.bilibili.com/h5-app/passport/login/scan?fixture=' + 'x' * 400
        with patch('tokenusage.bilibili_login._passport', return_value=({'qrcode_key': 'stub', 'url': url}, [])):
            with self.assertRaisesRegex(LoginUnavailable, '过长'):
                BilibiliQrLogin().start()
