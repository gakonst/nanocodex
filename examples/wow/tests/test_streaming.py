"""Protocol/projection evidence only; live WoW and account delivery are separate."""
import json
from pathlib import Path
import tempfile
import subprocess
import unittest
from transport.streaming import project, decode, StreamLimit
from transport.dispatch import Dispatcher


def delta(text, cursor='1', *, kind='assistant.delta'):
    return dict(type='event', cursor=cursor, turn_id='turn-1', event=dict(type=kind,
        payload=dict(text=text, model_call_index=0, item_id='answer-1', phase='final_answer')))

class ProjectionTests(unittest.TestCase):
    def record(self):
        return dict(turn='turn-1', outputs=[], done=False)

    def test_completion_without_snapshot_preserves_streamed_answer(self):
        for completion in ({}, {'final_message': None}):
            for canonical in (False, True):
                with self.subTest(completion=completion, canonical=canonical):
                    record = self.record()
                    project('r', record, delta('Hello '))
                    project('r', record, delta('Ω'))
                    if canonical:
                        project('r', record, delta('Hello Ω', kind='assistant.message'))
                    project('r', record, dict(type='turn_completed', **completion))
                    stream = record['streams'][record['answer_stream']]
                    self.assertEqual(stream['text'], 'Hello Ω')
                    self.assertEqual(stream['offset'], len('Hello Ω'.encode()))
                    self.assertTrue(stream['sealed'])
                    self.assertTrue(record['done'])
                    self.assertEqual([decode(o['value'])['op'] for o in record['outputs']],
                                     ['append', 'append', 'end', 'done'])

    def test_explicit_empty_completion_clears_streamed_answer(self):
        record = self.record()
        project('r', record, delta('draft', kind='assistant.message'))
        project('r', record, dict(type='turn_completed', final_message=''))
        stream = record['streams'][record['answer_stream']]
        self.assertEqual(stream['text'], '')
        self.assertEqual(stream['offset'], 0)
        self.assertTrue(record['done'])
        self.assertEqual([decode(o['value'])['op'] for o in record['outputs']],
                         ['append', 'reset', 'end', 'done'])

    def test_completion_without_any_answer_still_terminates(self):
        record = self.record()
        project('r', record, dict(type='turn_completed'))
        self.assertTrue(record['done'])
        self.assertEqual([decode(o['value'])['op'] for o in record['outputs']],
                         ['end', 'done'])
        self.assertEqual(next(iter(record['streams'].values()))['text'], '')

    def test_invalid_final_snapshot_does_not_mutate_answer(self):
        record = self.record()
        project('r', record, delta('answer'))
        before = json.dumps(record, sort_keys=True)
        with self.assertRaises(ValueError):
            project('r', record, dict(type='turn_completed', final_message=[]))
        self.assertEqual(json.dumps(record, sort_keys=True), before)

    def test_canonical_correction_resets_one_block_and_long_utf8_splits(self):
        record=self.record(); project('r',record,delta('draft'))
        final='é😀'*4000
        project('r',record,dict(type='turn_completed',final_message=final))
        decoded=[decode(o['value']) for o in record['outputs']]
        self.assertEqual(decoded[1]['op'],'reset')
        self.assertEqual(''.join(o['text'] for o in decoded[2:] if o['op']=='append'),final)
        self.assertTrue(all(len(o['value'].encode())<=4096 for o in record['outputs']))
        self.assertEqual(len({o['event_id'] for o in record['outputs']}),len(record['outputs']))

    def test_python_frames_reach_lua_incrementally_without_replay_duplication(self):
        from transport.dispatch import wire_fragments
        from transport.protocol import Frame
        record=self.record()
        for event in [delta('Hello '),delta('Ω!'),dict(type='turn_completed',final_message='Hello Ω!')]:
            project('request-1',record,event)
        frames=[]; seq=0
        for mid, output in enumerate(record['outputs'],1):
            for payload in wire_fragments(output,mid):
                seq+=1
                frames.append(Frame(19,seq,0,payload,True).encode().hex())
        result=subprocess.run(['lua','transport/test_streaming_pipeline.lua'],input='\n'.join(frames)+'\n',text=True,capture_output=True)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertIn('PASS:',result.stdout)

    def test_bound_is_explicit_and_child_text_is_not_root_reply(self):
        record=self.record(); event=delta('child'); event['agent_id']=2
        project('r',record,event); self.assertEqual(record['outputs'],[])
        with self.assertRaises(StreamLimit): project('r',record,delta('x'*262145))


class RestartAndReplayTests(unittest.TestCase):
    def setup_dispatcher(self, directory, backend):
        dispatcher = Dispatcher(backend, Path(directory) / 'journal')
        self.addCleanup(dispatcher.close)
        body = json.dumps(dict(schemaVersion=1, source='nanocodex-wow',
            type='nanocodex.ask', prompt='hello', mode='agent'))
        dispatcher.dispatch(body, 'request-1')
        return dispatcher

    def test_overlapping_batch_deduplicated_before_coalescing(self):
        class Store:
            rows = [delta('Hello ', '1')]
            def events(self, *args): return self.rows
        class Backend:
            store = Store()
            def handle(self, *args):
                return dict(thread_id='thread-1', turn_id='turn-1', status='queued')
        with tempfile.TemporaryDirectory() as directory:
            backend = Backend()
            dispatcher = self.setup_dispatcher(directory, backend)
            dispatcher.poll('request-1')
            backend.store.rows = [delta('Hello ', '1'), delta('world', '2'), delta('world', '2')]
            outputs = dispatcher.poll('request-1')
            text = ''.join(decode(o['value'])['text'] for o in outputs if o['kind'] == 'stream')
            self.assertEqual(text, 'Hello world')
            self.assertEqual(outputs, dispatcher.poll('request-1'))

    def test_open_shared_store_does_not_skip_waiting_thread_subscription(self):
        class Store:
            def events(self, *args): return [delta('cached', '1')]
        class Backend:
            store = Store()
            def __init__(self): self.clients = {'other-thread': object()}; self.calls = []
            def handle(self, *args):
                return dict(thread_id='thread-1', turn_id='turn-1', status='queued')
            def subscribe(self, thread):
                self.calls.append(thread)
                self.clients[thread] = object()
        with tempfile.TemporaryDirectory() as directory:
            backend = Backend()
            dispatcher = self.setup_dispatcher(directory, backend)
            outputs = dispatcher.poll('request-1')
            self.assertEqual(backend.calls, ['thread-1'])
            self.assertEqual(decode(outputs[-1]['value'])['text'], 'cached')
            self.assertEqual(outputs, dispatcher.poll('request-1'))
            self.assertEqual(backend.calls, ['thread-1'])

    def test_cached_completion_remains_available_without_subscription(self):
        class Store:
            def events(self, *args):
                return [dict(type='turn_completed', id='turn-1', cursor='1', final_message='offline')]
        class Backend:
            store = Store()
            clients = {}
            def handle(self, *args):
                return dict(thread_id='thread-1', turn_id='turn-1', status='queued')
            def subscribe(self, thread): raise AssertionError('Cached completion needs no network')
        with tempfile.TemporaryDirectory() as directory:
            dispatcher = self.setup_dispatcher(directory, Backend())
            outputs = dispatcher.poll('request-1')
            self.assertEqual(outputs[-1]['state'], 'completed')

class ToolHeavyStressTests(unittest.TestCase):
    def test_128_tools_and_commentary_blocks_keep_the_final_answer(self):
        record = dict(turn='stress-turn', outputs=[], done=False)
        for i in range(128):
            payload = dict(text=f'Tool {i+1}/128', model_call_index=i, item_id=f'c{i}', phase='commentary')
            for kind in ('assistant.delta', 'assistant.message'):
                project('stress', record, dict(type='event', event=dict(type=kind, payload=payload)))
            for kind in ('tool.call', 'tool.result'):
                project('stress', record, dict(type='event', event=dict(type=kind, payload=dict(
                    tool='exec_command', call_id=f'call{i}', status='completed', arguments={'private':'not for display'}, result='x'*100000))))
        project('stress', record, dict(type='turn_completed', final_message='STRESS PASS Ω'))
        self.assertTrue(record['done'])
        self.assertEqual(len(record['streams']), 129)
        self.assertEqual(record['streams'][record['answer_stream']]['text'], 'STRESS PASS Ω')
        tools = [o for o in record['outputs'] if o['kind']=='ack']
        self.assertEqual(len(tools), 256)
        self.assertTrue(all('private' not in o['value'] and len(o['value'])<180 for o in tools))
        self.assertEqual(len({o['event_id'] for o in record['outputs']}), len(record['outputs']))
        self.assertEqual(record['outputs'][-1]['state'], 'completed')

    def test_tool_activity_is_bounded_and_child_events_do_not_leak(self):
        record=dict(turn='t',outputs=[],done=False)
        project('r',record,dict(type='event',agent_id=2,event=dict(type='tool.call',payload={'tool':'child'})))
        self.assertEqual(record['outputs'],[])
        project('r',record,dict(type='event',event=dict(type='tool.result',payload={'tool':'bad\n|Ttexture|t','status':'failed'})))
        self.assertEqual(record['outputs'][0]['value'],'ncm1\ttool\tr\tt\ttool\tfailed')
