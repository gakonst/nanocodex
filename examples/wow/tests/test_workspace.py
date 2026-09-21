"""Authenticated workspace snapshot and durable account-fence contracts, no network."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from urllib.parse import unquote

from durable_client import DurableBackend, DurableClient
from server import APIError, Backend, metadata
from transport.dispatch import Dispatcher, MAX_ROWS, MAX_CATALOG_BYTES


def action(name='refresh_projects', **fields):
    return json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow',
                      'type': 'nanocodex.action', 'action': name, **fields})


class WorkspaceTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name)
        home = patch('server.Path.home', return_value=self.home)
        home.start()
        self.addCleanup(home.stop)
        credentials = patch.object(Backend, 'credentials', return_value='synthetic-account-a')
        self.credentials = credentials.start()
        self.addCleanup(credentials.stop)

    def backend(self, durable=False):
        backend = DurableBackend(storage_dir=self.home / 'durable') if durable else Backend()
        backend.base_url = 'https://example.invalid'
        if durable:
            self.addCleanup(backend.close)
        return backend

    def dispatcher(self, backend, name='dispatch'):
        dispatcher = Dispatcher(backend, self.home / (name + '.sqlite3'))
        self.addCleanup(dispatcher.close)
        return dispatcher

    def upstream(self, backend, roster):
        opening = patch.object(backend.opener, 'open')
        opened = opening.start()
        self.addCleanup(opening.stop)
        opened.return_value.__enter__.return_value.read.return_value = json.dumps(roster).encode()
        return opened

    def test_one_authenticated_agents_read_independent_of_project_count(self):
        for durable in (False, True):
            for count in (1, 8, 100, 600):
                with self.subTest(durable=durable, projects=count):
                    backend = self.backend(durable)
                    roster = {f'p{i}': {'title': 'Project Ω ' + str(i)} for i in range(count)}
                    roster.update({f't{i}': {'project_root_id': f'p{i}', 'title': 'Chat 🎮 ' * 45}
                                   for i in range(count)})
                    with metadata(backend) as (store, local):
                        local['threads']['t0'] = {'closed': True, 'title': 'Local closed chat'}
                        local['threads']['deleted'] = {'title': 'Must not leak'}
                        store.save(local)
                    opening = self.upstream(backend, {'data': list(roster), 'summaries': roster})
                    d = self.dispatcher(backend, f'dispatch-{durable}-{count}')
                    first = d.dispatch(action(page=0), 'snapshot')
                    self.assertEqual([o['kind'] for o in first], ['ack', 'projects'])
                    opening.assert_called_once()
                    request = opening.call_args.args[0]
                    self.assertEqual((request.method, request.full_url), ('GET', 'https://example.invalid/v1/agents'))
                    self.assertEqual(request.get_header('Authorization'), 'Bearer synthetic-account-a')
                    self.assertEqual(request.get_header('User-agent'), 'Nanocodex-WoW/0.1')
                    # Mutate the upstream response; continuations must use the saved strings.
                    opening.return_value.__enter__.return_value.read.return_value = b'{"data":[]}'
                    pages = [first[1]['value']]
                    for page in range(1, int(first[0]['value'].split('\t')[4])):
                        out = d.dispatch(action(snapshot_id='snapshot', page=page), f'page:{page}')
                        pages.append(out[1]['value'])
                    rows = [list(map(unquote, line.split('\t'))) for page in pages for line in page.splitlines()[1:]]
                    self.assertEqual(len(rows), count * 3)
                    self.assertIn(['T', 'p0', 't0', 'Local closed chat', 'closed'], rows)
                    self.assertNotIn('Must not leak', str(rows))
                    opening.assert_called_once()
                    self.assertLessEqual(sum(len(page.encode()) for page in pages), MAX_CATALOG_BYTES)
                    if count == 600:
                        self.assertGreater(len(pages), 32, 'roster continuation has a larger page budget than history')
                    if durable:
                        self.assertEqual(backend.clients, {})  # Catalog reads don't subscribe every thread.

    def test_six_hundred_independent_projects_preserve_all_twelve_hundred_rows(self):
        backend = self.backend(durable=True)
        roster = {f'p{i}': {'title': 'Synthetic project ' + str(i)} for i in range(600)}
        opening = self.upstream(backend, {'data': list(roster), 'summaries': roster})
        d = self.dispatcher(backend)
        first = d.dispatch(action(page=0), 'catalog')
        self.assertEqual(first[-1]['kind'], 'projects')
        pages = d._read('catalog')[1]['snapshot']['pages']
        rows = [line.split('\t') for page in pages for line in page.splitlines()[1:]]
        self.assertEqual(len(rows), 1200)
        self.assertEqual(len([row for row in rows if row[0] == 'P']), 600)
        self.assertEqual(len([row for row in rows if row[0] == 'T']), 600)
        opening.assert_called_once()

    def test_workspace_strict_query_and_missing_auth_do_no_network(self):
        backend = self.backend()
        opening = self.upstream(backend, {'data': []})
        for query in ({'closed': ['true']}, {'project_id': ['p']}, {'token': ['private']}):
            with self.assertRaises(APIError) as caught:
                backend.handle('GET', '/api/workspace', query, {})
            self.assertEqual(caught.exception.status, 400)
        self.credentials.side_effect = APIError('Sign in', 401)
        with self.assertRaises(APIError) as caught:
            backend.handle('GET', '/api/workspace', {}, {})
        self.assertEqual(caught.exception.status, 401)
        opening.assert_not_called()

    def test_combined_row_capacity_boundary_is_explicit_and_atomic(self):
        backend = self.backend()
        d = self.dispatcher(backend)
        # Every independent conversation contributes one P and one T row.
        roster = {f'p{i}': {'title': 'P'} for i in range(MAX_ROWS // 2)}
        opening = self.upstream(backend, {'data': list(roster), 'summaries': roster})
        out = d.dispatch(action(page=0), 'at-limit')
        self.assertEqual(out[-1]['kind'], 'projects')
        snapshot = d._read('at-limit')[1]['snapshot']
        self.assertEqual(sum(len(page.splitlines()) - 1 for page in snapshot['pages']), MAX_ROWS)
        roster['one-more-thread'] = {'project_root_id': 'p0', 'title': 'T'}
        opening.return_value.__enter__.return_value.read.return_value = json.dumps({'data': list(roster), 'summaries': roster}).encode()
        out = d.dispatch(action(page=0), 'over-limit')
        self.assertEqual([(o['kind'], o['state'], o['value']) for o in out],
                         [('error', 'error', 'Dispatcher capacity exceeded.')])
        self.assertNotIn('snapshot', d._read('over-limit')[1])
        self.assertEqual(opening.call_count, 2)
        self.assertEqual(d.dispatch(action(page=0), 'over-limit'), out)
        # A failed refresh cannot replace or destroy a previously retained roster.
        previous = d.dispatch(action(snapshot_id='at-limit', page=0), 'previous-page')
        self.assertEqual(previous[-1]['value'], snapshot['pages'][0])
        self.assertEqual(d._read('at-limit')[1]['snapshot'], snapshot)
        self.assertEqual(opening.call_count, 2)

    def test_retained_byte_limit_counts_all_page_headers(self):
        backend = self.backend()
        d = self.dispatcher(backend)
        self.upstream(backend, {'data': ['p'], 'summaries': {'p': {'title': 'X' * 30}}})
        # Force two small pages, then test the exact sum of retained UTF-8 bytes.
        with patch('transport.dispatch.MAX_VALUE', 64):
            d.dispatch(action(page=0), 'measure')
            pages = d._read('measure')[1]['snapshot']['pages']
            self.assertEqual(len(pages), 2)
            size = sum(len(page.encode()) for page in pages)
            with patch('transport.dispatch.MAX_CATALOG_BYTES', size):
                self.assertEqual(d.dispatch(action(page=0), 'at-bytes')[-1]['kind'], 'projects')
            with patch('transport.dispatch.MAX_CATALOG_BYTES', size - 1):
                out = d.dispatch(action(page=0), 'over-bytes')
                self.assertEqual(out[0]['value'], 'Dispatcher capacity exceeded.')
                self.assertEqual(out[0]['state'], 'error')
                self.assertNotIn('snapshot', d._read('over-bytes')[1])

    def test_durable_account_switch_fences_rest_stop_history_workspace_and_resume(self):
        backend = self.backend(durable=True)
        backend._open_store()
        d = self.dispatcher(backend)
        opening = self.upstream(backend, {'data': []})
        self.credentials.return_value = 'synthetic-account-b'
        for name, fields in (('refresh_projects', {'page': 0}),
                             ('load_history', {'thread_id': 't', 'view_id': 'v'}),
                             ('stop_turn', {'thread_id': 't', 'turn_id': 'turn'}),
                             ('reconnect', {})):
            with self.subTest(action=name):
                out = d.dispatch(action(name, **fields), name)
                self.assertEqual(out[0]['state'], 'error')
                self.assertIn('Sign in', out[0]['value'])
        self.assertEqual(d.dispatch(action('connection_status'), 'status')[0]['value'],
                         'ncm1\tconnection\tdisconnected')
        opening.assert_not_called()
        self.assertEqual(backend.clients, {})

    def test_reconnect_restores_missing_subscriptions_without_restarting_blocked_clients(self):
        backend = self.backend(durable=True)
        with patch.object(DurableClient, 'start') as start:
            client = backend.subscribe('blocked-thread')
            client.status = 'blocked'
            with backend.store.db:
                backend.store.db.execute('INSERT INTO subscriptions VALUES(?)', ('missing-thread',))
            opening = self.upstream(backend, {'data': ['blocked-thread', 'missing-thread']})
            d = self.dispatcher(backend)
            out = d.dispatch(action('reconnect'), 'resume')
            self.assertEqual(out[0]['value'], 'ncm1\tconnection\tconnected')
            self.assertIs(backend.clients['blocked-thread'], client)
            self.assertEqual(client.status, 'blocked')
            self.assertIn('missing-thread', backend.clients)
            self.assertEqual(start.call_count, 2)
            opening.assert_called_once()
            self.assertEqual(backend.store.pending('blocked-thread'), [])
            self.assertEqual(backend.store.pending('missing-thread'), [])
