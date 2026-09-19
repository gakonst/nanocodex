"""Deterministic, local text-to-addon-command planner. Never executes commands."""
from decimal import Decimal, InvalidOperation
import re

# Kept deliberately narrower than WoW's complete action/keyboard vocabulary.
ACTIONS = {'TOGGLEWORLDMAP'}
ACTION_NAMES = {'map': 'TOGGLEWORLDMAP', 'world map': 'TOGGLEWORLDMAP'}
MODIFIERS = ('CTRL', 'ALT', 'SHIFT')
BASE_KEYS = set('ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789') | {
    'SPACE', 'TAB', 'ESCAPE', 'BACKSPACE', 'ENTER', 'UP', 'DOWN', 'LEFT', 'RIGHT',
    'HOME', 'END', 'INSERT', 'DELETE', 'PAGEUP', 'PAGEDOWN',
} | {'F' + str(i) for i in range(1, 25)} | {'BUTTON' + str(i) for i in range(1, 6)} | {'NUMPAD' + str(i) for i in range(10)}
EXAMPLES = ('Supported examples: bind map to shift m; bind SHIFT-M to TOGGLEWORLDMAP; '
            'unbind SHIFT-M; set UI scale to 85%; enable enemy nameplates; '
            'disable enemy nameplates; undo settings.')


def key_name(text):
    if not isinstance(text, str) or not re.fullmatch(r'[A-Za-z0-9 +\-]+', text):
        raise ValueError('Invalid binding key. ' + EXAMPLES)
    parts = re.split(r'[ +\-]+', text.strip().upper())
    aliases = {'CONTROL': 'CTRL', 'ESC': 'ESCAPE', 'RETURN': 'ENTER'}
    parts = [aliases.get(p, p) for p in parts]
    if not parts or parts[-1] not in BASE_KEYS:
        raise ValueError('Unsupported binding key. ' + EXAMPLES)
    modifiers = parts[:-1]
    if any(p not in MODIFIERS for p in modifiers) or len(set(modifiers)) != len(modifiers):
        raise ValueError('Invalid key modifiers. ' + EXAMPLES)
    return '-'.join([m for m in MODIFIERS if m in modifiers] + [parts[-1]])


def plan(text):
    if not isinstance(text, str) or not 1 <= len(text) <= 500 or not text.strip():
        raise ValueError('Enter a short settings request. ' + EXAMPLES)
    # No command fragments or control characters are accepted, even when the
    # rest of a request looks valid. Never copy arbitrary input into commands.
    if not re.fullmatch(r'[A-Za-z0-9 .%+\-]+', text):
        raise ValueError('Unsupported characters in settings request. ' + EXAMPLES)
    normalized = ' '.join(text.strip().split())
    lowered = normalized.lower()
    warnings = ['Review the preview in WoW before applying. Changes require the installed Nanocodex addon and cannot be applied during combat.']
    if lowered == 'undo settings':
        return {'summary': 'Undo the last settings change saved by the addon.',
                'preview_command': '', 'apply_command': '/nc settings undo',
                'undo_command': '/nc settings undo',
                'warnings': ['Undo is limited to the last change retained by the addon.']}
    match = re.fullmatch(r'bind (.+) to (.+)', normalized, re.IGNORECASE)
    if match:
        left, right = match.groups()
        if left.lower() in ACTION_NAMES:
            action, key = ACTION_NAMES[left.lower()], key_name(right)
        else:
            key, action = key_name(left), right.upper()
        if action not in ACTIONS:
            raise ValueError('Unsupported binding action. ' + EXAMPLES)
        command = 'bind ' + key + ' ' + action
        summary = 'Bind ' + key + ' to ' + action + '.'
        warnings.append('This replaces any action currently assigned to that key.')
    elif lowered.startswith('unbind '):
        key = key_name(normalized[7:])
        command, summary = 'unbind ' + key, 'Remove the binding on ' + key + '.'
    elif (match := re.fullmatch(r'set ui scale to ([0-9]+(?:\.[0-9]+)?)(%)?', lowered)):
        try:
            scale = Decimal(match[1]) / (100 if match[2] else 1)
        except InvalidOperation:
            raise ValueError('Invalid UI scale. ' + EXAMPLES)
        if not Decimal('0.64') <= scale <= Decimal('1.0'):
            raise ValueError('UI scale must be between 64% and 100% (0.64 to 1.0).')
        encoded = format(scale.normalize(), 'f')
        command = 'hud scale ' + encoded
        summary = 'Set UI scale to ' + format((scale * 100).normalize(), 'f') + '%.'
    elif (match := re.fullmatch(r'(enable|disable) enemy nameplates', lowered)):
        enabled = match[1] == 'enable'
        command = 'hud nameplates ' + ('on' if enabled else 'off')
        summary = ('Enable' if enabled else 'Disable') + ' enemy nameplates.'
    else:
        raise ValueError('This settings request is not supported. ' + EXAMPLES)
    return {'summary': summary, 'preview_command': '/nc settings preview ' + command,
            'apply_command': '/nc settings apply ' + command,
            'undo_command': '/nc settings undo', 'warnings': warnings}
