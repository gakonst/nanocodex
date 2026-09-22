import unittest
from unittest.mock import patch
import settings
import server


class SettingsTests(unittest.TestCase):
    def test_requested_binding_forms_agree(self):
        for text in ('bind map to shift m', 'bind SHIFT-M to TOGGLEWORLDMAP', 'bind world map to SHIFT+M'):
            result = settings.plan(text)
            self.assertEqual(result['preview_command'], '/nc settings preview bind SHIFT-M TOGGLEWORLDMAP')
            self.assertEqual(result['apply_command'], '/nc settings apply bind SHIFT-M TOGGLEWORLDMAP')
            self.assertEqual(result['undo_command'], '/nc settings undo')

    def test_canonical_keys(self):
        self.assertEqual(settings.key_name('shift control alt m'), 'CTRL-ALT-SHIFT-M')
        self.assertEqual(settings.key_name('ctrl + F12'), 'CTRL-F12')
        self.assertEqual(settings.plan('unbind SHIFT-M')['apply_command'], '/nc settings apply unbind SHIFT-M')
        for key in ('SHIFT-SHIFT-M', 'M-SHIFT', 'F99', 'CTRL', 'BUTTON99', 'META-M', 'SHIFT--'):
            with self.assertRaises(ValueError, msg=key):
                settings.key_name(key)

    def test_scale_formats_and_bounds(self):
        for value in ('85%', '0.85'):
            self.assertEqual(settings.plan('set UI scale to ' + value)['apply_command'], '/nc settings apply hud scale 0.85')
        self.assertEqual(settings.plan('set UI scale to 100%')['summary'], 'Set UI scale to 100%.')
        for value in ('5%', '101%', '85', 'nan', 'inf', '1e3'):
            with self.assertRaises(ValueError):
                settings.plan('set UI scale to ' + value)

    def test_nameplates_and_undo(self):
        for request, suffix in (('enable', 'on'), ('disable', 'off')):
            self.assertEqual(settings.plan(request + ' enemy nameplates')['apply_command'], '/nc settings apply hud nameplates ' + suffix)
        result = settings.plan('undo settings')
        self.assertEqual(result['apply_command'], '/nc settings undo')
        self.assertEqual(result['preview_command'], '')

    def test_injection_and_unsupported_actions(self):
        for text in ('bind SHIFT-M to RunScript', 'bind SHIFT-M to TOGGLEWORLDMAP; /run evil()', 'bind map to SHIFT-M\n/run evil()', 'bind map to "M"', 'bind map to M\x00', 'set UI scale to 0.85\r', 'enable enemy nameplates and delete files', 'turn on everything'):
            with self.assertRaises(ValueError, msg=text):
                settings.plan(text)

    def test_local_endpoint_never_accesses_account(self):
        backend = server.Backend()
        with patch.object(backend, 'request', side_effect=AssertionError('network access')), patch.object(backend, 'credentials', side_effect=AssertionError('auth access')):
            result = backend.handle('POST', '/api/settings/plan', {}, {'text': 'bind map to shift m'})
        self.assertEqual(result['apply_command'], '/nc settings apply bind SHIFT-M TOGGLEWORLDMAP')
        with self.assertRaises(server.APIError) as caught:
            backend.handle('POST', '/api/settings/plan', {}, {'text': 'execute arbitrary Lua'})
        self.assertIn('Supported examples:', caught.exception.message)

if __name__ == '__main__':
    unittest.main()
