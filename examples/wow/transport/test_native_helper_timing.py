"""Execute the native event sequence against fake Wayland/XKB and a fake clock."""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


FAKE_API = r'''
#ifndef CARRIER_FAKE_API_H
#define CARRIER_FAKE_API_H
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
struct wl_display { int unused; };
struct wl_registry { int unused; };
struct wl_seat { int unused; };
struct wl_interface { int unused; };
struct zwp_virtual_keyboard_manager_v1 { int unused; };
struct zwp_virtual_keyboard_v1 { int unused; };
struct xkb_context { int unused; };
struct xkb_keymap { int unused; };
struct xkb_rule_names { const char *rules, *model, *layout, *variant, *options; };
typedef uint32_t xkb_mod_index_t;
struct wl_registry_listener {
 void (*global)(void *, struct wl_registry *, uint32_t, const char *, uint32_t);
 void (*remove)(void *, struct wl_registry *, uint32_t);
};
static struct wl_display display;
static struct wl_registry registry;
static struct wl_seat fake_seat;
static struct wl_interface wl_seat_interface, zwp_virtual_keyboard_manager_v1_interface;
static struct zwp_virtual_keyboard_manager_v1 fake_manager;
static struct zwp_virtual_keyboard_v1 fake_keyboard;
static struct xkb_context context;
static struct xkb_keymap keymap;
static uint64_t elapsed_us, last_release_us, modifier_release_us;
static unsigned roundtrips, symbols, active_key, last_key, mask, ctrl, shift;
#define XKB_MOD_NAME_CTRL "Control"
#define XKB_MOD_NAME_SHIFT "Shift"
#define XKB_MOD_NAME_ALT "Mod1"
#define XKB_KEYMAP_FORMAT_TEXT_V1 1
#define WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1 1
#define WL_KEYBOARD_KEY_STATE_PRESSED 1
#define WL_KEYBOARD_KEY_STATE_RELEASED 0
#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 1
#endif
static struct wl_display *wl_display_connect(const char *name) { return &display; }
static struct wl_registry *wl_display_get_registry(struct wl_display *d) { return &registry; }
static void *wl_registry_bind(struct wl_registry *r, uint32_t id, const struct wl_interface *i, uint32_t v) {
 return i == &wl_seat_interface ? (void *)&fake_seat : (void *)&fake_manager;
}
static void wl_registry_add_listener(struct wl_registry *r, const struct wl_registry_listener *l, void *data) {
 l->global(data,r,1,"wl_seat",1);
 l->global(data,r,2,"zwp_virtual_keyboard_manager_v1",1);
}
static int wl_display_roundtrip(struct wl_display *d) { roundtrips++; elapsed_us += 5000; return 0; }
static struct xkb_context *xkb_context_new(int flags) { return &context; }
static struct xkb_keymap *xkb_keymap_new_from_names(struct xkb_context *c, const struct xkb_rule_names *n, int flags) { return &keymap; }
static xkb_mod_index_t xkb_keymap_mod_get_index(struct xkb_keymap *m, const char *name) {
 return !strcmp(name,XKB_MOD_NAME_CTRL) ? 0 : !strcmp(name,XKB_MOD_NAME_SHIFT) ? 1 : 2;
}
static char *xkb_keymap_get_as_string(struct xkb_keymap *m, int format) {
 char *s = malloc(2); assert(s); strcpy(s,"x"); return s;
}
static void xkb_keymap_unref(struct xkb_keymap *m) {}
static void xkb_context_unref(struct xkb_context *c) {}
static struct zwp_virtual_keyboard_v1 *zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(struct zwp_virtual_keyboard_manager_v1 *m, struct wl_seat *s) { return &fake_keyboard; }
static void zwp_virtual_keyboard_v1_keymap(struct zwp_virtual_keyboard_v1 *k, int format, int fd, size_t size) {}
static void zwp_virtual_keyboard_v1_modifiers(struct zwp_virtual_keyboard_v1 *k, uint32_t depressed, uint32_t latched, uint32_t locked, uint32_t group) {
 assert(latched == 0 && locked == 0 && group == 0); mask = depressed;
}
static void zwp_virtual_keyboard_v1_key(struct zwp_virtual_keyboard_v1 *k, uint32_t time, uint32_t code, uint32_t state) {
 if (code == 29 || code == 42) {
  if (!state && !modifier_release_us) modifier_release_us = elapsed_us;
  if (code == 29) ctrl = state; else shift = state;
  return;
 }
 assert(ctrl && shift && mask == 3);
 assert(code != 62); /* Never F4. */
 if (state) { assert(active_key == 0); active_key = code; }
 else { assert(active_key == code); active_key = 0; last_release_us = elapsed_us; last_key = code; symbols++; }
}
static void zwp_virtual_keyboard_v1_destroy(struct zwp_virtual_keyboard_v1 *k) {
 assert(active_key == 0 && ctrl == 0 && shift == 0 && mask == 0);
}
static void wl_display_disconnect(struct wl_display *d) {
 printf("{\"symbols\":%u,\"roundtrips\":%u,\"tail_us\":%llu,\"last_key\":%u}\n",
        symbols, roundtrips, (unsigned long long)(modifier_release_us-last_release_us), last_key);
}
static int fake_memfd_create(const char *name, unsigned flags) { return 7; }
static ssize_t fake_write(int fd, const void *data, size_t len) { return (ssize_t)len; }
static int fake_close(int fd) { return 0; }
static int fake_usleep(useconds_t delay) { elapsed_us += delay; return 0; }
static int fake_clock_gettime(clockid_t id, struct timespec *t) {
 t->tv_sec = elapsed_us / 1000000; t->tv_nsec = (elapsed_us % 1000000) * 1000; return 0;
}
#define memfd_create fake_memfd_create
#define write fake_write
#define close fake_close
#define usleep fake_usleep
#define clock_gettime fake_clock_gettime
#endif
'''


@unittest.skipUnless(shutil.which('cc'), 'C compiler unavailable')
class NativeHelperTimingTests(unittest.TestCase):
    def test_full_packet_keeps_modifiers_until_tail_settle(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'xkbcommon').mkdir()
            (root / 'fake.h').write_text(FAKE_API)
            for name in ('wayland-client.h', 'xkbcommon/xkbcommon.h', 'virtual-keyboard.h'):
                (root / name).write_text('#include "fake.h"\n')
            source = Path(__file__).parent / 'native' / 'carrier-keys.c'
            (root / source.name).write_bytes(source.read_bytes())
            binary = root / 'carrier-test'
            subprocess.run(['cc', '-std=c99', '-I', str(root), str(root / source.name), '-o', str(binary)],
                           check=True, capture_output=True, timeout=30)
            for count in (47, 335):
                for hold in (0, 1, 5):
                    with self.subTest(symbols=count, hold=hold):
                        args = [str(binary), str(hold), 'C9'] + ['C1'] * (count - 2) + ['C10']
                        result = subprocess.run(args, capture_output=True, text=True, check=True, timeout=2)
                        trace = json.loads(result.stdout)
                        self.assertEqual(trace['symbols'], count)
                        self.assertEqual(trace['roundtrips'], 2 * count + 5)
                        self.assertEqual(trace['last_key'], 87)
                        # Exclude the last release's fake 5ms roundtrip and the
                        # configured release hold: these do not supply the tail.
                        self.assertGreaterEqual(trace['tail_us'] - 5000 - hold * 1000, 40000)
                        self.assertLessEqual(trace['tail_us'], 50000)
