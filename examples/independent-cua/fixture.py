"""Owned native GTK fixture: UI clicks are the only way to advance revision."""
import json
import os
import sys
from pathlib import Path
import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk, GLib, Gdk

root, lane = Path(sys.argv[1]), int(sys.argv[2])
state = dict(pid=os.getpid(), lane=lane, revision=0, completed=[], keys=0, clicks=0)
window = Gtk.Window(title='Independent CUA '+('foreground' if lane < 0 else str(lane)))
window.set_default_size(340, 220)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=16)
box.set_border_width(20)
window.add(box)
label = Gtk.Label(label='Owned fixture')
box.pack_start(label, True, True, 0)
button = Gtk.Button(label='Advance revision')
box.pack_start(button, True, True, 0)

def save():
    allocation = button.get_allocation()
    x, y = button.translate_coordinates(window, 0, 0)
    state['buttons'] = {'advance': [x+allocation.width/2, y+allocation.height/2]}
    label.set_text(f'Lane {lane}: revision {state["revision"]}; keys {state["keys"]}')
    temporary = root / f'lane-{lane}.tmp'
    temporary.write_text(json.dumps(state))
    temporary.replace(root / f'lane-{lane}.json')
    return False

def clicked(*_):
    state['completed'].append(dict(choice='advance', revision=state['revision']))
    state['revision'] += 1
    state['clicks'] += 1
    GLib.idle_add(save)

def key(*_):
    state['keys'] += 1
    GLib.idle_add(save)
    return False

button.connect('clicked', clicked)
window.connect('key-press-event', key)
window.connect('destroy', Gtk.main_quit)
window.show_all()
GLib.timeout_add(100, lambda: (save(), True)[1])
Gtk.main()
