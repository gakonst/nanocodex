import http.client
import json
import os
import shutil
import subprocess
from pathlib import Path
import tempfile
import types
import threading
import unittest
import urllib.error
from unittest.mock import patch
import server


class FakeBackend(server.Backend):
    def __init__(self):
        self.calls = []
        self.base_url = 'https://example.invalid'
        self.response = {}
    def request(self, method, path, payload=None, key=None):
        self.calls.append((method, path, payload, key))
        if method == 'POST':
            return {'agent_id': 'agent-1', 'turn_id': 'turn-1'}
        return self.response


class BackendTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        home = patch.object(server.Path, 'home', return_value=Path(temporary.name))
        home.start(); self.addCleanup(home.stop)
        credentials = patch.object(FakeBackend, 'credentials', return_value='test-account')
        credentials.start(); self.addCleanup(credentials.stop)

    def test_new_run_exact_shape_and_idempotency(self):
        b = FakeBackend()
        result = b.handle('POST', '/api/send', {}, {'text': 'Help', 'mode': 'build', 'context': {'edition': 'Classic'}, 'idempotency_key': 'stable-1'})
        method, path, body, key = b.calls[0]
        self.assertEqual((method, path, key), ('POST', '/v1/agent-runs', 'stable-1'))
        self.assertEqual(body['settings'], {'model': 'gpt-5.6-luna', 'thinking': 'low', 'reasoning_mode': 'standard', 'fast_mode': False})
        self.assertIn('untrusted game data', body['input'])
        self.assertIn('browse current primary sources', body['input'])
        self.assertEqual(result['thread_id'], 'agent-1')
        self.assertEqual(len(b.calls), 1)

    def test_new_run_against_upstream_source_validator(self):
        validator = Path(__file__).resolve().parents[2] / 'nanocodex-api-reference/js/managed/src/agent-settings.ts'
        bun = shutil.which('bun')
        if not bun or not validator.is_file():
            self.skipTest('Optional upstream source checkout and Bun required')
        b = FakeBackend()
        b.handle('POST', '/api/send', {}, {'text': 'Help', 'mode': 'hint'})
        script = ('import {parseAgentRunBody} from ' + json.dumps(str(validator)) + ';'
                  'const body = await Bun.stdin.text(); parseAgentRunBody(body);'
                  'let rejected=false; try {parseAgentRunBody(JSON.stringify({settings:{model:"gpt-5.6-luna"},input:"test"}));} catch {rejected=true;}'
                  'if(!rejected) throw new Error("incomplete settings unexpectedly accepted");')
        result = subprocess.run([bun, '-e', script], input=json.dumps(b.calls[0][2]), text=True, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_resume_persistent_thread(self):
        b = FakeBackend()
        b.handle('POST', '/api/send', {}, {'text': 'Next hint', 'mode': 'hint', 'thread_id': 'existing', 'idempotency_key': 'same'})
        self.assertEqual(b.calls[0][0:2], ('POST', '/v1/agents/existing/turns'))
        self.assertEqual(b.calls[0][3], 'same')
        self.assertIn('reveal progressively', b.calls[0][2]['input'])
        self.assertEqual(len(b.calls), 1)

    def test_agent_mode_preserves_configuration_and_raw_task(self):
        b = FakeBackend()
        b.handle('POST', '/api/send', {}, {'text': 'Review the code', 'mode': 'agent', 'thread_id': 'existing'})
        self.assertEqual(len(b.calls), 1)
        self.assertEqual(b.calls[0][:3], ('POST', '/v1/agents/existing/turns', {'input': 'Review the code'}))

    def test_game_mode_without_selected_thread_creates_luna(self):
        b = FakeBackend()
        b.handle('POST', '/api/send', {}, {'text': 'Give a hint', 'mode': 'hint', 'project_id': 'root'})
        self.assertEqual(len(b.calls), 1)
        self.assertEqual(b.calls[0][1], '/v1/agent-runs')
        self.assertEqual(b.calls[0][2]['settings'], {'model': 'gpt-5.6-luna', 'thinking': 'low', 'reasoning_mode': 'standard', 'fast_mode': False})

    def test_projection_uses_real_membership_unknown_status(self):
        b = FakeBackend()
        b.response = {'data': ['root', 'child', 'solo'], 'summaries': {'root': {'title': 'WoW', 'project_root_id': 'root', 'project_name': 'WoW'}, 'child': {'project_root_id': 'root', 'project_title': 'Build'}, 'solo': {'title': 'Solo'}}}
        self.assertEqual(len(b.handle('GET', '/api/projects', {}, None)['projects']), 2)
        threads = b.handle('GET', '/api/threads', {'project_id': ['root']}, None)['threads']
        self.assertEqual([t['id'] for t in threads], ['root', 'child'])
        self.assertEqual(threads[1]['status'], 'unknown')

    def test_messages_filter_tools_and_duplicate_final(self):
        b = FakeBackend()
        b.response = {'data': [{'type': 'turn_accepted', 'input': 'Hello'}, {'type': 'event', 'turn_id': 't', 'event': {'type': 'assistant.message', 'payload': {'text': 'Done'}}}, {'type': 'event', 'event': {'type': 'tool.result', 'payload': {'text': 'secret'}}}, {'type': 'turn_completed', 'id': 't', 'final_message': 'Done'}], 'has_more': True}
        result = b.handle('GET', '/api/messages', {'thread_id': ['agent-1']}, None)
        self.assertEqual(result['messages'], [{'role': 'user', 'text': 'Hello'}, {'role': 'assistant', 'text': 'Done'}])
        self.assertTrue(result['has_more'])

    def test_credentials_private_store(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / 'account.json'
            p.write_text(json.dumps({'version': 1, 'accounts': {'https://example.invalid': {'api_key': 'private-value'}}}))
            p.chmod(0o600)
            with patch.dict(os.environ, {'NANOCODEX_ACCOUNT_FILE': str(p), 'NANOCODEX_MANAGED_URL': 'https://example.invalid'}):
                b = server.Backend()
                self.assertEqual(b.credentials(), 'private-value')
                p.chmod(0o644)
                with self.assertRaises(server.APIError):
                    b.credentials()
                p.chmod(0o600)
                target = Path(tmp) / 'link'
                target.symlink_to(p)
                with patch.dict(os.environ, {'NANOCODEX_ACCOUNT_FILE': str(target)}):
                    with self.assertRaises(server.APIError):
                        b.credentials()

    def test_transport_failure_does_not_retry_write_or_leak(self):
        b = server.Backend()
        with patch.object(b, 'credentials', return_value='PRIVATE'), patch.object(b.opener, 'open', side_effect=urllib.error.URLError('PRIVATE')) as opening:
            with self.assertRaises(server.APIError) as caught:
                b.request('POST', '/v1/agent-runs', {'input': 'hello'}, 'stable')
            self.assertEqual(opening.call_count, 1)
            self.assertNotIn('PRIVATE', caught.exception.message)
            req = opening.call_args.args[0]
            self.assertEqual(req.get_header('Idempotency-key'), 'stable')
            self.assertEqual(req.get_header('Authorization'), 'Bearer PRIVATE')
            self.assertEqual(req.get_header('User-agent'), 'Nanocodex-WoW/0.1')

    def test_redirect_is_not_followed(self):
        with self.assertRaises(server.APIError):
            server.NoRedirect().redirect_request(None, None, None, None, None, 'https://evil.test')

    def test_invalid_input_performs_no_writes(self):
        for data in ({'text': 'x', 'thread_id': '../escape'}, {'text': 'x', 'model': 'other'}, {'text': 'x', 'mode': 'bogus'}, {'text': ''}):
            b = FakeBackend()
            with self.assertRaises(server.APIError):
                b.handle('POST', '/api/send', {}, data)
            self.assertEqual(b.calls, [])


class HTTPTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.backend = FakeBackend()
        cls.httpd = server.make_server(0, cls.backend)
        cls.port = cls.httpd.server_address[1]
        cls.thread = threading.Thread(target=cls.httpd.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()
        cls.httpd.server_close()
        cls.thread.join()

    def request(self, path='/api/status', method='GET', body=None, headers=None):
        conn = http.client.HTTPConnection('127.0.0.1', self.port)
        conn.request(method, path, body=body, headers=headers or {})
        response = conn.getresponse()
        result = response.status, response.read(), dict(response.getheaders())
        conn.close()
        return result

    def test_uppercase_game_assets_and_font_mime(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'web').mkdir()
            (root / 'web' / 'FONT.TTF').write_bytes(b'font-fixture')
            (root / 'web' / 'TEXTURE.PNG').write_bytes(b'image-fixture')
            with patch.object(server, 'ROOT', root):
                status, body, headers = self.request('/FONT.TTF')
                self.assertEqual((status, body, headers['Content-Type']), (200, b'font-fixture', 'font/ttf'))
                self.assertEqual(self.request('/TEXTURE.PNG')[0], 200)

    def test_same_origin_status(self):
        status, body, headers = self.request(headers={'Origin': f'http://127.0.0.1:{self.port}'})
        self.assertEqual(status, 200)
        self.assertTrue(json.loads(body)['connected'])
        self.assertNotIn('Access-Control-Allow-Origin', headers)
        self.assertEqual(headers['Cache-Control'], 'no-store')

    def test_rebinding_and_cross_origin_blocked(self):
        for headers in ({'Host': f'evil.test:{self.port}'}, {'Origin': 'https://evil.test'}, {'Origin': 'null'}, {'Sec-Fetch-Site': 'cross-site'}):
            self.assertEqual(self.request(headers=headers)[0], 403)

    def test_body_bounds_and_type(self):
        self.assertEqual(self.request('/api/send', 'POST', '{}', {'Content-Length': '65537', 'Content-Type': 'application/json'})[0], 413)
        self.assertEqual(self.request('/api/send', 'POST', '{}', {'Content-Type': 'text/plain'})[0], 415)
        self.assertEqual(self.request('/api/send', 'POST', '[]', {'Content-Type': 'application/json'})[0], 400)

    def test_local_audio_transcription(self):
        calls = []
        def transcribe(audio, mime):
            calls.append((audio, mime))
            return 'Hello Azeroth'
        with patch.dict('sys.modules', {'voice': types.SimpleNamespace(transcribe=transcribe)}):
            status, body, _ = self.request('/api/transcribe', 'POST', b'audio-bytes', {'Content-Type': 'audio/webm'})
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), {'text': 'Hello Azeroth'})
        self.assertEqual(calls, [(b'audio-bytes', 'audio/webm')])

    def test_json_base64_audio(self):
        calls = []
        def transcribe(audio, mime):
            calls.append((audio, mime))
            return 'Azeroth'
        with patch.dict('sys.modules', {'voice': types.SimpleNamespace(transcribe=transcribe)}):
            status, body, _ = self.request('/api/transcribe', 'POST', json.dumps({'audio': 'YWJj', 'mime': 'audio/webm'}), {'Content-Type': 'application/json'})
            self.assertEqual(status, 200)
            self.assertEqual(calls, [(b'abc', 'audio/webm')])
            self.assertEqual(self.request('/api/transcribe', 'POST', '{"audio":"???", "mime":"audio/webm"}', {'Content-Type': 'application/json'})[0], 400)

    def test_audio_limit_and_failure_sanitization(self):
        self.assertEqual(self.request('/api/transcribe', 'POST', b'x', {'Content-Length': str(server.MAX_AUDIO + 1), 'Content-Type': 'audio/webm'})[0], 413)
        def transcribe(audio, mime):
            raise RuntimeError('private-path-and-details')
        with patch.dict('sys.modules', {'voice': types.SimpleNamespace(transcribe=transcribe)}):
            status, body, _ = self.request('/api/transcribe', 'POST', b'x', {'Content-Type': 'audio/webm'})
        self.assertEqual(status, 503)
        self.assertNotIn(b'private-path', body)

    def test_token_import_disabled(self):
        status, body, _ = self.request('/api/connect', 'POST', '{"token":"DO-NOT-ECHO"}', {'Content-Type': 'application/json'})
        self.assertEqual(status, 405)
        self.assertNotIn(b'DO-NOT-ECHO', body)

    def test_path_traversal_blocked(self):
        self.assertEqual(self.request('/%2e%2e/server.py')[0], 404)

    def test_unhandled_error_sanitized(self):
        with patch.object(self.backend, 'handle', side_effect=RuntimeError('SECRET')):
            status, body, _ = self.request()
        self.assertEqual(status, 502)
        self.assertNotIn(b'SECRET', body)


if __name__ == '__main__':
    unittest.main()

class DeployedAdmissionFallbackTests(unittest.TestCase):
    def test_explicit_missing_combined_route_uses_same_key(self):
        class Backend(FakeBackend):
            def request(self, method, path, payload=None, key=None):
                self.calls.append((method,path,payload,key))
                if path == '/v1/agent-runs': raise server.APIError('missing',404)
                return {'agent_id':'agent-1'} if path == '/v1/agents' else {'turn_id':'turn-1'}
        b=Backend()
        receipt=b.handle('POST','/api/send',{},dict(text='hello',mode='hint',idempotency_key='stable'))
        self.assertEqual(receipt['turn_id'],'turn-1')
        self.assertEqual([x[1] for x in b.calls],['/v1/agent-runs','/v1/agents','/v1/agents/agent-1/turns'])
        self.assertEqual([x[3] for x in b.calls],['stable']*3)
        self.assertEqual(b.calls[0][2]['input'],b.calls[2][2]['input'])
        self.assertEqual(b.calls[0][2]['settings'],b.calls[1][2]['settings'])

    def test_uncertain_combined_admission_never_falls_back(self):
        for status in (401,403,409,429,502,503):
            class Backend(FakeBackend):
                def request(self,*args):
                    self.calls.append(args)
                    raise server.APIError('unconfirmed',status)
            b=Backend()
            with self.assertRaises(server.APIError):
                b.handle('POST','/api/send',{},dict(text='hello',mode='hint',idempotency_key='stable'))
            self.assertEqual(len(b.calls),1)

    def test_unconfirmed_create_does_not_submit_turn(self):
        class Backend(FakeBackend):
            def request(self,method,path,payload=None,key=None):
                self.calls.append(path)
                raise server.APIError('missing' if path=='/v1/agent-runs' else 'unknown',404 if path=='/v1/agent-runs' else 502)
        b=Backend()
        with self.assertRaises(server.APIError):
            b.handle('POST','/api/send',{},dict(text='hello',mode='hint',idempotency_key='stable'))
        self.assertEqual(b.calls,['/v1/agent-runs','/v1/agents'])
