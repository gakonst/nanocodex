"""Fake subprocess only: these tests never access or inject the live desktop."""
import json
import subprocess
import unittest
from unittest.mock import patch
from types import SimpleNamespace
from wayland import Desktop


def keymap(start=191):
    return {start+n-13: (0xffbe+n-1, 0) for n in range(13,25)}


class X11Tests(unittest.TestCase):
    def make(self, *, active=None, title='World of Warcraft\n', pid='123\n', focus='456\n', mapping=None, binds=None, failure=None, race=False):
        calls=[]
        active = {'address':'0x123','class':'steam_app_battlenet','pid':123,'xwayland':True} if active is None else active
        reader=patch('wayland.keyboard_mapping', return_value=keymap() if mapping is None else mapping,
                     side_effect=OSError('mapping unavailable') if failure=='mapping' else None)
        reader.start(); self.addCleanup(reader.stop)
        injected=False
        def run(args, **kwargs):
            nonlocal injected
            calls.append((args, kwargs))
            self.assertTrue(kwargs['check'])
            self.assertLessEqual(kwargs['timeout'], 2)
            if failure and args[0] == failure:
                raise subprocess.TimeoutExpired(args, 1)
            if args[:2] == ['hyprctl','-j']:
                result = ({} if race and injected else active) if args[2]=='activewindow' else (binds or [])
                return SimpleNamespace(stdout=json.dumps(result).encode())
            if args[:2] == ['xdotool','key']:
                injected=True
                return SimpleNamespace(stdout=b'')
            result = {'getwindowfocus':focus,'getwindowname':title,'getwindowpid':pid}[args[1]]
            return SimpleNamespace(stdout=result.encode())
        return Desktop('0x123','steam_app_battlenet',0,0,4,run=run,input_backend='x11'), calls

    def injections(self, calls):
        return [args for args, _ in calls if args[:2] == ['xdotool','key']]

    def test_one_bounded_numeric_press_release_batch(self):
        desktop,calls=self.make()
        keys=['F21']+['F13']*333+['F22']
        self.assertTrue(desktop.send_keys(keys))
        self.assertEqual(self.injections(calls), [['xdotool','key','--delay','0','199']+['191']*333+['200']])
        self.assertFalse(any(args[0]=='wtype' or '--window' in args or 'windowfocus' in args or 'windowactivate' in args or '--clearmodifiers' in args for args,_ in calls))
        self.assertEqual(sum(args == ['xdotool','getwindowpid','456'] for args,_ in calls), 2)

    def test_identity_mismatches_fail_closed(self):
        for changes in ({'title':'Other'}, {'title':'World of Warcraft '}, {'pid':'124'}, {'pid':'0'}, {'focus':'0'}, {'focus':'invalid'}, {'active':{}}, {'active':{'address':'0x123','class':'steam_app_battlenet','pid':123,'xwayland':False}}, {'active':{'address':'other','class':'steam_app_battlenet','pid':123,'xwayland':True}}):
            with self.subTest(changes=changes):
                desktop,calls=self.make(**changes)
                self.assertFalse(desktop.send_keys(['F13']))
                self.assertEqual(self.injections(calls), [])

    def test_mapping_is_complete_unmodified_and_numeric_label_consistent(self):
        for mapping in ({}, {**keymap(),202:(0xffff,0)}, {**keymap(),191:(0,0xffca)}):
            desktop,calls=self.make(mapping=mapping)
            self.assertFalse(desktop.send_keys(['F13']))
            self.assertEqual(self.injections(calls), [])

    def test_actual_mapping_binding_and_evdev_offset_rejected(self):
        for binding in ({'key':'F13'}, {'keycode':191}, {'key':'ANY'}, {'keycode':220}, {'keycode':212}, {'key':'code:220'}):
            desktop,calls=self.make(mapping=keymap(220),binds=[binding])
            self.assertFalse(desktop.send_keys(['F13']))
            self.assertEqual(self.injections(calls), [])

    def test_nonstandard_existing_codes_are_used_without_symbol_lookup(self):
        desktop,calls=self.make(mapping=keymap(8))
        self.assertTrue(desktop.send_keys(['F13','F24']))
        self.assertEqual(self.injections(calls), [['xdotool','key','--delay','0','008','019']])

    def test_query_errors_fail_closed(self):
        for command in ('mapping','hyprctl','xdotool'):
            desktop,calls=self.make(failure=command)
            self.assertFalse(desktop.send_keys(['F13']))
            self.assertEqual(self.injections(calls), [])

    def test_post_input_focus_race_reports_failure(self):
        desktop,calls=self.make(race=True)
        self.assertFalse(desktop.send_keys(['F13']))
        self.assertEqual(len(self.injections(calls)), 1)

    def test_invalid_input_and_backend(self):
        desktop,calls=self.make()
        for keys in ([],['W'],['F13']*336):
            with self.assertRaises(ValueError): desktop.send_keys(keys)
        self.assertEqual(calls, [])
        for options in ({'input_backend':'auto'}, {'input_backend':'x11','window_title':''}):
            with self.assertRaises(ValueError): Desktop('a','b',0,0,4,**options)


if __name__ == '__main__': unittest.main()
