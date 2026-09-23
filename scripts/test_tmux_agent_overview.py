import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('overview', Path(__file__).with_name('tmux-agent-overview.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class OverviewTests(unittest.TestCase):
    def row(self, metadata):
        return '\t'.join(['%3', '$1', 'synthetic', '0', '1', 'zsh', 'Terminal', json.dumps(metadata)])

    def test_expiry_malformed_future_and_ordinary_panes(self):
        for metadata in ({}, [], {'version': 1, 'updated_at': 1}, {'version': 1, 'updated_at': 30000}):
            rows = m.parse_panes(self.row(metadata), 20000)
            self.assertEqual(m.describe(rows[0], {}), ('zsh', 'Terminal', ''))
        self.assertEqual(m.parse_panes('invalid'), [])

    def test_metadata_fallback_and_presentation(self):
        row = m.parse_panes(self.row({'version': 1, 'updated_at': 19000, 'agent_id': 'demo', 'status': 'running', 'prompt': 'Newest request'}), 20000)[0]
        self.assertEqual(m.describe(row, None), ('running', 'demo', 'Newest request'))
        detail = m.describe(row, {'demo': {'presentation': {'title': 'Review tests', 'activity': 'I am checking results', 'lastUserPrompt': 'Old request'}}})
        self.assertEqual(detail, ('running · I am checking results', 'Review tests', 'Newest request'))
        row['metadata']['status'] = 'idle'
        self.assertNotIn('checking', m.describe(row, {'demo': {'presentation': {'activity': 'checking'}}})[0])

    def test_sanitizes_control_sequences_and_bounds_text(self):
        self.assertNotIn('\x1b', m.clean('\x1b[31m\ntext'))
        self.assertEqual(len(m.clean('x' * 1000)), 512)

    def test_jump_uses_explicit_client_and_validated_pane(self):
        row = m.parse_panes(self.row({}), 20000)[0]
        with patch.object(m, 'command') as command:
            m.jump(row, '/dev/ttys999')
        self.assertEqual(command.call_args.args, ('tmux', 'switch-client', '-c', '/dev/ttys999', '-t', '$1', ';', 'select-window', '-t', '%3', ';', 'select-pane', '-t', '%3'))

    def test_list_failure_is_nonfatal(self):
        with patch.object(m, 'command', side_effect=FileNotFoundError):
            self.assertIsNone(m.load_summaries('missing'))

if __name__ == '__main__':
    unittest.main()
