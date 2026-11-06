#!/usr/bin/env python3
"""Offline PR media tests. No real network, gh invocation or credential reads."""
import importlib.util
import io
import json
from pathlib import Path
import socket
import ssl
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('fetch', Path(__file__).resolve().parents[1] / 'Resources/Helpers/pr_video_fetch.py')
fetch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetch)
ASSET = 'https://github.com/user-attachments/assets/test-123'
CDN = 'https://' + fetch.CDN + '/123/clip.mp4?signature=synthetic'


class Connection:
    def close(self):
        pass


class Response(io.BytesIO):
    def __init__(self, code, body=b'', **headers):
        super().__init__(body)
        self.status, self.headers = code, headers

    def getheader(self, name, default=None):
        return self.headers.get(name, default)


class Fetch(unittest.TestCase):
    def test_review_pagination_diff_state_and_incomplete_responses(self):
        calls = []
        info = {'title': 'Synthetic ñ', 'state': 'open', 'draft': False, 'merged': False,
                'base': {'ref': 'main', 'sha': '111'}, 'head': {'ref': 'feature', 'sha': '222'},
                'body': 'Review fixture', 'changed_files': 101, 'additions': 101, 'deletions': 0}
        def invoke(args, limit, seconds=20):
            calls.append(args)
            if '/files?' not in args[-1]:
                return json.dumps(info).encode()
            n = 1 if args[-1].endswith('page=2') else 100
            return json.dumps([{'filename': f'file-{i}.rs', 'status': 'added',
                                'patch': '@@ -0 +1 @@\n+answer', 'additions': 1, 'deletions': 0}
                               for i in range(n)]).encode()
        text = fetch.review('synthetic/repo', 1, invoke)
        self.assertIn('Estado: open', text)
        self.assertIn('Head: feature  222', text)
        self.assertIn('+answer', text)
        self.assertEqual(len(calls), 3)
        self.assertTrue(all(c[0:5] == ['api', '--hostname', 'github.com', '--method', 'GET'] for c in calls))
        with self.assertRaises(RuntimeError):
            fetch.review('bad/../repo', 1, invoke)
        def incomplete(args, limit, seconds=20):
            return b'[]' if '/files?' in args[-1] else json.dumps(info).encode()
        with self.assertRaises(RuntimeError):
            fetch.review('synthetic/repo', 1, incomplete)
        info['changed_files'] = 3001
        with self.assertRaises(RuntimeError):
            fetch.review('synthetic/repo', 1, invoke)

    def test_url_policy_rejects_external_destinations_and_downgrades(self):
        for url in [ASSET, CDN, 'https://user-images.githubusercontent.com/123/v.webm']:
            fetch.target(url)
        for url in ['http://github.com/user-attachments/assets/test',
                    'https://github.com@evil.test/user-attachments/assets/test',
                    'https://github.com:8443/user-attachments/assets/test',
                    'https://github.com.evil.test/user-attachments/assets/test',
                    'https://github.com/user-attachments/files/%2e%2e/private',
                    'https://github.com/user-attachments/assets/test%0d%0aHeader',
                    'https://127.0.0.1/video.mp4', 'file:///tmp/video.mp4',
                    'https://example.s3.amazonaws.com/clip.mp4', ASSET + '#fragment']:
            with self.subTest(url=url), self.assertRaises(RuntimeError):
                fetch.target(url)
        with self.assertRaises(RuntimeError):
            fetch.target(CDN, initial=True)

    def test_dns_rejects_private_mixed_loopback_and_transition_addresses(self):
        def resolve(ips):
            return lambda *args, **kwargs: [(socket.AF_INET6 if ':' in ip else socket.AF_INET, socket.SOCK_STREAM, 6, '', (ip, 443)) for ip in ips]
        self.assertEqual(fetch.public_addresses('github.com', resolve(['140.82.112.3'])), ['140.82.112.3'])
        for ips in [['127.0.0.1'], ['10.0.0.1'], ['169.254.169.254'], ['100.64.1.1'],
                    ['::1'], ['fc00::1'], ['fe80::1'], ['::ffff:140.82.112.3'],
                    ['2002:7f00:0001::'], ['140.82.112.3', '192.168.1.1']]:
            with self.subTest(ips=ips), self.assertRaises(RuntimeError):
                fetch.public_addresses('github.com', resolve(ips))

    def test_connection_pins_the_verified_ip_and_preserves_tls_hostname(self):
        connection = fetch.PinnedHTTPS('github.com', timeout=3, context=ssl.create_default_context())
        context = unittest.mock.Mock()
        connection._context = context
        with patch.object(fetch, 'public_addresses', return_value=['140.82.112.3']), patch.object(fetch.socket, 'create_connection') as connect:
            connection.connect()
            self.assertEqual(connect.call_count, 1)
            self.assertEqual(connect.call_args.args[0], ('140.82.112.3', 443))
            self.assertTrue(0 < connect.call_args.args[1] <= 3)
            context.wrap_socket.assert_called_once_with(connect.return_value, server_hostname='github.com')

    def test_auth_is_only_requested_after_denial_and_never_forwarded_cross_host(self):
        responses = [Response(404), Response(302, Location=CDN),
                     Response(200, b'video', **{'Content-Type': 'video/mp4', 'Content-Length': '5'})]
        requests, auth = [], []
        def send(url, headers, timeout):
            requests.append((url.hostname, dict(headers)))
            return Connection(), responses.pop(0)
        def token():
            auth.append(True)
            return 'synthetic-secret'
        with tempfile.TemporaryDirectory(prefix='tw-pr-video-test-', dir='/private/tmp') as path:
            source = fetch.download(ASSET, Path(path), send, token)
            self.assertEqual(source.read_bytes(), b'video')
        self.assertEqual(auth, [True])
        self.assertNotIn('Authorization', requests[0][1])
        self.assertEqual(requests[1][1]['Authorization'], 'token synthetic-secret')
        self.assertNotIn('Authorization', requests[2][1])

    def test_a_cross_host_bounce_cannot_reacquire_credentials(self):
        responses = [Response(302, Location=CDN), Response(302, Location=ASSET), Response(404)]
        requests = []
        def send(url, headers, timeout):
            requests.append(headers)
            return Connection(), responses.pop(0)
        def no_token():
            self.fail('cross-host redirect reacquired a credential')
        with tempfile.TemporaryDirectory(prefix='tw-pr-video-test-', dir='/private/tmp') as path, self.assertRaises(RuntimeError):
            fetch.download(ASSET, Path(path), send, no_token)
        self.assertTrue(all('Authorization' not in h for h in requests))

    def test_redirects_are_validated_before_the_next_request(self):
        calls = []
        def send(url, headers, timeout):
            calls.append(url.hostname)
            return Connection(), Response(302, Location='https://127.0.0.1/private')
        with tempfile.TemporaryDirectory(prefix='tw-pr-video-test-', dir='/private/tmp') as path, self.assertRaises(RuntimeError):
            fetch.download(ASSET, Path(path), send)
        self.assertEqual(calls, ['github.com'])

    def test_streamed_size_and_content_type_limits_without_trusting_headers(self):
        for headers, body in [({'Content-Type': 'text/html'}, b'<html>'),
                              ({'Content-Type': 'video/mp4'}, b'x' * 17),
                              ({'Content-Type': 'video/mp4', 'Content-Length': '4'}, b'x'),
                              ({'Content-Type': 'video/mp4'}, b'')]:
            with tempfile.TemporaryDirectory(prefix='tw-pr-video-test-', dir='/private/tmp') as path, patch.object(fetch, 'MAX_INPUT', 16), self.assertRaises(RuntimeError):
                fetch.download(ASSET, Path(path), lambda *args: (Connection(), Response(200, body, **headers)))

    def test_metadata_paginates_without_losing_comments_or_exposing_auth(self):
        cursors = []
        def invoke(command, limit, input_bytes):
            self.assertEqual(command[:2], ['api', 'graphql'])
            data = json.loads(input_bytes)
            cursors.append(data['variables']['cursor'])
            page = {'body': 'body', 'comments': {'nodes': [{'body': 'first' if len(cursors) == 1 else 'second'}],
                    'pageInfo': {'hasNextPage': len(cursors) == 1, 'endCursor': 'cursor-1'}}}
            return json.dumps({'data': {'repository': {'pullRequest': page}}}).encode()
        self.assertEqual(fetch.metadata('test/repo', 7, invoke), ['body', 'first', 'second'])
        self.assertEqual(cursors, [None, 'cursor-1'])

    def test_metadata_rejects_unsafe_targets_and_excessive_text(self):
        def never(*args, **kwargs):
            self.fail('invalid target reached gh')
        for repo, number in [('test/..', 1), ('a/b;echo', 1), ('test/repo', 0), ('test/repo', True)]:
            with self.assertRaises(RuntimeError):
                fetch.metadata(repo, number, never)
        def invoke(*args, **kwargs):
            return json.dumps({'data': {'repository': {'pullRequest': {'body': '123456', 'comments': {
                'nodes': [], 'pageInfo': {'hasNextPage': False}}}}}}).encode()
        with patch.object(fetch, 'MAX_METADATA', 5), self.assertRaises(RuntimeError):
            fetch.metadata('test/repo', 1, invoke)


if __name__ == '__main__':
    unittest.main()
