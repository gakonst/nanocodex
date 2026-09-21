import json
import os
import subprocess
import sys
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch
import server


class AccountBackend(server.Backend):
    def __init__(self, key='account-a', origin='https://example.invalid'):
        self.key = key
        self.base_url = origin
        self.roster = {'root': {'title': 'Native root', 'project_root_id': 'root', 'project_name': 'Native project'},
                       'child': {'title': 'Native child', 'project_root_id': 'root', 'project_name': 'Native project'}}
        self.receipts = {}
        self.calls = []
    def credentials(self):
        return self.key
    def request(self, method, path, payload=None, key=None):
        self.calls.append((method, path, payload, key))
        if method == 'GET':
            return {'data': list(self.roster), 'summaries': self.roster}
        if key not in self.receipts:
            agent = 'created-' + str(len(self.receipts))
            self.receipts[key] = agent
            self.roster[agent] = {}
        return {'agent_id': self.receipts[key]}


class LocalOrganizationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        home = patch.object(server.Path, 'home', return_value=self.home)
        home.start(); self.addCleanup(home.stop)
        self.backend = AccountBackend()

    def call(self, path, data=None, query=None, backend=None):
        return (backend or self.backend).handle('POST' if data is not None else 'GET', path, query or {}, data)

    def file(self):
        return next((self.home / '.local/share/nanocodex-wow').glob('*.json'))

    def test_create_project_and_empty_chat_persist_native_ids(self):
        project = self.call('/api/projects/create', {'name': ' Local ', 'idempotency_key': 'project'})
        chat = self.call('/api/threads/create', {'project_id': project['project_id'], 'title': 'A chat', 'idempotency_key': 'chat'})
        self.assertEqual(project['project_id'], project['thread_id'])
        self.assertEqual(chat['project_id'], project['project_id'])
        self.assertEqual(project['metadata_scope'], 'local_companion')
        posts = [c for c in self.backend.calls if c[0] == 'POST']
        self.assertEqual(len(posts), 2)
        self.assertTrue(all(c[1:3] == ('/v1/agents', {'settings': server.LUNA_SETTINGS}) for c in posts))
        fresh = AccountBackend()
        fresh.roster = self.backend.roster
        projects = self.call('/api/projects', backend=fresh)['projects']
        self.assertIn({'id': 'root', 'name': 'Native project'}, projects)
        self.assertIn({'id': project['project_id'], 'name': 'Local'}, projects)
        rows = self.call('/api/threads', query={'project_id': [project['project_id']]}, backend=fresh)['threads']
        self.assertEqual([r['id'] for r in rows], [project['thread_id'], chat['thread_id']])
        self.assertEqual(rows[-1]['title'], 'A chat')
        self.assertEqual(self.file().stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.file().parent.stat().st_mode & 0o777, 0o700)
        self.assertNotIn('account-a', self.file().read_text())
        self.assertNotIn(self.file().stem, json.dumps(project))

    def test_local_overrides_close_restore_and_native_membership(self):
        self.call('/api/projects/update', {'project_id': 'root', 'name': 'Renamed project'})
        self.call('/api/threads/update', {'thread_id': 'child', 'title': 'Renamed child'})
        self.call('/api/threads/close', {'thread_id': 'child'})
        self.assertEqual(self.call('/api/projects')['projects'], [{'id': 'root', 'name': 'Renamed project'}])
        self.assertEqual([r['id'] for r in self.call('/api/threads', query={'project_id': ['root']})['threads']], ['root'])
        closed = self.call('/api/threads', query={'project_id': ['root'], 'closed': ['true']})['threads']
        self.assertEqual(closed, [{'id': 'child', 'title': 'Renamed child', 'status': 'unknown', 'closed': True}])
        self.call('/api/threads/restore', {'thread_id': 'child'})
        self.assertEqual(self.call('/api/threads', query={'project_id': ['root'], 'closed': ['true']})['threads'], [])
        self.assertTrue(all(c[0] == 'GET' for c in self.backend.calls))

    def test_credentials_and_origin_isolate_metadata(self):
        self.call('/api/projects/update', {'project_id': 'root', 'name': 'Private'})
        for other in (AccountBackend('account-b'), AccountBackend(origin='https://other.invalid')):
            self.assertEqual(self.call('/api/projects', backend=other)['projects'][0]['name'], 'Native project')
        self.assertEqual(self.call('/api/projects')['projects'][0]['name'], 'Private')

    def test_create_replay_after_restart_preserves_later_changes(self):
        body = {'name': 'First', 'idempotency_key': 'stable'}
        first = self.call('/api/projects/create', body)
        self.call('/api/projects/update', {'project_id': first['project_id'], 'name': 'Later'})
        self.call('/api/threads/close', {'thread_id': first['thread_id']})
        fresh = AccountBackend(); fresh.roster = self.backend.roster
        replay = self.call('/api/projects/create', body, backend=fresh)
        self.assertEqual(first['thread_id'], replay['thread_id'])
        self.assertEqual(replay['name'], 'Later')
        self.assertTrue(all(c[0] == 'GET' for c in fresh.calls))
        with self.assertRaises(server.APIError) as error:
            self.call('/api/projects/create', {**body, 'name': 'Different'})
        self.assertEqual(error.exception.status, 409)
        del fresh.roster[first['thread_id']]
        with self.assertRaises(server.APIError) as error:
            self.call('/api/projects/create', body, backend=fresh)
        self.assertEqual(error.exception.status, 404)

    def test_accepted_create_save_failure_reconciles_same_key(self):
        real_save = server.LocalMetadata.save
        count = 0
        def fail_second(store, value):
            nonlocal count
            count += 1
            if count == 2: raise OSError('private storage detail')
            real_save(store, value)
        body = {'name': 'Project', 'idempotency_key': 'stable'}
        with patch.object(server.LocalMetadata, 'save', fail_second), self.assertRaises(server.APIError) as error:
            self.call('/api/projects/create', body)
        self.assertEqual(error.exception.status, 503)
        self.assertNotIn('private storage detail', error.exception.message)
        accepted = self.call('/api/projects/create', body)
        self.assertEqual(accepted['thread_id'], 'created-0')
        self.assertEqual(len(self.backend.receipts), 1)
        self.assertEqual([c[3] for c in self.backend.calls if c[0] == 'POST'], ['stable', 'stable'])

    def test_uncertain_create_not_retried_automatically(self):
        original = self.backend.request
        def uncertain(method, *args, **kwargs):
            result = original(method, *args, **kwargs)
            if method == 'POST': raise server.APIError('Uncertain', 502)
            return result
        body = {'name': 'Project', 'idempotency_key': 'stable'}
        with patch.object(self.backend, 'request', uncertain), self.assertRaises(server.APIError):
            self.call('/api/projects/create', body)
        self.assertEqual(len([c for c in self.backend.calls if c[0] == 'POST']), 1)
        self.assertEqual(self.call('/api/projects/create', body)['thread_id'], 'created-0')
        self.assertEqual(len(self.backend.receipts), 1)

    def test_concurrent_same_key_creates_once(self):
        results, errors = [], []
        def create():
            try: results.append(self.call('/api/projects/create', {'name': 'P', 'idempotency_key': 'same'}))
            except Exception as exc: errors.append(exc)
        threads = [threading.Thread(target=create) for _ in range(5)]
        for thread in threads: thread.start()
        for thread in threads: thread.join()
        self.assertEqual(errors, [])
        self.assertEqual(len(results), 5)
        self.assertEqual(len([c for c in self.backend.calls if c[0] == 'POST']), 1)

    def test_concurrent_updates_do_not_lose_records(self):
        errors = []
        def rename(agent):
            try: self.call('/api/threads/update', {'thread_id': agent, 'title': agent + '-new'})
            except Exception as exc: errors.append(exc)
        threads = [threading.Thread(target=rename, args=(agent,)) for agent in ('root', 'child')]
        for thread in threads: thread.start()
        for thread in threads: thread.join()
        self.assertEqual(errors, [])
        self.assertEqual(set(json.loads(self.file().read_text())['threads']), {'root', 'child'})

    def test_atomic_replace_failure_preserves_prior_state(self):
        self.call('/api/projects/update', {'project_id': 'root', 'name': 'Original'})
        original = self.file().read_bytes()
        with patch.object(server.os, 'replace', side_effect=OSError('disk failure')):
            with self.assertRaises(server.APIError) as error:
                self.call('/api/projects/update', {'project_id': 'root', 'name': 'Changed'})
        self.assertEqual(error.exception.status, 503)
        self.assertEqual(self.file().read_bytes(), original)
        self.assertEqual(list(self.file().parent.glob('*.tmp')), [])

    def test_separate_process_updates_are_locked(self):
        script = """
import sys
from pathlib import Path
from unittest.mock import patch
import server
class Backend(server.Backend):
    base_url = 'https://example.invalid'
    def __init__(self): pass
    def credentials(self): return 'account-a'
    def agents(self): return {'data': ['root', 'child'], 'summaries': {}}
with patch.object(server.Path, 'home', return_value=Path(sys.argv[1])):
    for i in range(10):
        Backend().handle('POST', '/api/threads/update', {}, {'thread_id':sys.argv[2], 'title':sys.argv[2] + str(i)})
"""
        processes = [subprocess.Popen([sys.executable, '-B', '-c', script, str(self.home), agent],
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE) for agent in ('root', 'child')]
        for process in processes:
            out, err = process.communicate(timeout=20)
            self.assertEqual(process.returncode, 0, err.decode())
        saved = json.loads(self.file().read_text())['threads']
        self.assertEqual(saved['root']['title'], 'root9')
        self.assertEqual(saved['child']['title'], 'child9')

    def test_corrupt_oversized_and_unsafe_files_fail_closed(self):
        self.call('/api/projects/update', {'project_id': 'root', 'name': 'Private'})
        path = self.file()
        original = path.read_bytes()
        for content in (b'{', b'x' * (server.MAX_METADATA + 1), b'{"version":1}', b'{"version":1,"projects":{"root":[]},"threads":{},"creates":{}}'):
            path.write_bytes(content)
            with self.assertRaises(server.APIError) as error: self.call('/api/projects')
            self.assertEqual(error.exception.status, 503)
            self.assertEqual(path.read_bytes(), content)
        path.write_bytes(original); path.chmod(0o644)
        with self.assertRaises(server.APIError): self.call('/api/projects')
        path.chmod(0o600)
        target = self.home / 'target'; target.write_bytes(original); target.chmod(0o600)
        path.unlink(); path.symlink_to(target)
        with self.assertRaises(server.APIError): self.call('/api/projects')
        self.assertEqual(target.read_bytes(), original)

    def test_symlink_directory_and_lock_rejected(self):
        self.call('/api/projects')
        directory = self.home / '.local/share/nanocodex-wow'
        lock = next(directory.glob('*.lock'))
        lock.unlink(); lock.symlink_to(self.home / 'target')
        with self.assertRaises(server.APIError): self.call('/api/projects')
        lock.unlink(); directory.rmdir()
        target = self.home / 'elsewhere'; target.mkdir()
        directory.symlink_to(target, target_is_directory=True)
        with self.assertRaises(server.APIError): self.call('/api/projects')
        self.assertEqual(list(target.iterdir()), [])

    def test_deleted_ids_cannot_mutate_and_stale_metadata_is_hidden(self):
        self.call('/api/threads/update', {'thread_id': 'child', 'title': 'Private'})
        original = self.file().read_bytes()
        del self.backend.roster['child']
        with self.assertRaises(server.APIError) as error:
            self.call('/api/threads/close', {'thread_id': 'child'})
        self.assertEqual(error.exception.status, 404)
        self.assertEqual(self.file().read_bytes(), original)
        self.assertEqual([r['id'] for r in self.call('/api/threads', query={'project_id': ['root']})['threads']], ['root'])

    def test_invalid_names_filters_and_missing_auth(self):
        for name in ('', ' ', 'x' * 161, None):
            with self.assertRaises(server.APIError): self.call('/api/projects/create', {'name': name, 'idempotency_key': 'k'})
        for closed in ('', 'yes', '1'):
            with self.assertRaises(server.APIError): self.call('/api/threads', query={'project_id': ['root'], 'closed': [closed]})
        self.assertEqual(self.backend.calls, [])
        with patch.object(self.backend, 'credentials', side_effect=server.APIError('Sign in', 401)):
            with self.assertRaises(server.APIError) as error: self.call('/api/projects')
            self.assertEqual(error.exception.status, 401)
        self.assertFalse((self.home / '.local').exists())
