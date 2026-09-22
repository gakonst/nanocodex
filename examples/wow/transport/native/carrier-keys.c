#define _GNU_SOURCE
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "virtual-keyboard.h"
static struct wl_seat *seat;
static struct zwp_virtual_keyboard_manager_v1 *manager;
static void add(void *data, struct wl_registry *r, uint32_t id, const char *iface, uint32_t version) {
 (void)data; (void)version;
 if(!strcmp(iface,"wl_seat")&&!seat) seat=wl_registry_bind(r,id,&wl_seat_interface,1);
 if(!strcmp(iface,"zwp_virtual_keyboard_manager_v1")) manager=wl_registry_bind(r,id,&zwp_virtual_keyboard_manager_v1_interface,1);
}
static void rem(void *d, struct wl_registry *r, uint32_t id) {(void)d;(void)r;(void)id;}
static const struct wl_registry_listener listener={add,rem};
static uint32_t now(void) {struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return (uint32_t)(t.tv_sec*1000+t.tv_nsec/1000000);}
int main(int argc,char **argv) {
 if(argc==2 && !strcmp(argv[1],"--describe")) {
  puts("{\"schema\":1,\"modifiers\":[\"CTRL\",\"SHIFT\"],\"keys\":[\"F1\",\"F2\",\"F3\",\"F5\",\"F6\",\"F7\",\"F8\",\"F9\",\"F10\",\"F11\"]}");return 0;
 }
 if(argc<3 || argc>892) {fprintf(stderr,"usage: carrier-keys HOLD_MS C1..C10 (max890)\n");return 2;}
 char *end;long hold=strtol(argv[1],&end,10);if(*end||hold<0||hold>50)return 2;
 if(argv[2][0]!='C')return 2;
 int chord=1;
 const unsigned chord_codes[10]={59,60,61,63,64,65,66,67,68,87};
 unsigned keys[890];for(int i=2;i<argc;i++){
  if(argv[i][0]!=(chord?'C':'F'))return 2;
  long n=strtol(argv[i]+1,&end,10);
  if(*end||(chord?(n<1||n>10):(n<13||n>24)))return 2;
  keys[i-2]=chord?chord_codes[n-1]:(183+(unsigned)n-13);
 }

 struct wl_display *d=wl_display_connect(NULL);if(!d)return 3;
 struct wl_registry *r=wl_display_get_registry(d);wl_registry_add_listener(r,&listener,NULL);
 if(wl_display_roundtrip(d)<0||!seat||!manager)return 4;
 struct xkb_context *c=xkb_context_new(0);if(!c)return 5;
 struct xkb_rule_names names={"evdev","pc105","us","",""};
 struct xkb_keymap *map=xkb_keymap_new_from_names(c,&names,0);if(!map)return 5;
 xkb_mod_index_t ci=xkb_keymap_mod_get_index(map,XKB_MOD_NAME_CTRL),si=xkb_keymap_mod_get_index(map,XKB_MOD_NAME_SHIFT),ai=xkb_keymap_mod_get_index(map,XKB_MOD_NAME_ALT);
 if(ci>=32||si>=32||ai>=32)return 5;
 uint32_t mods=(1u<<ci)|(1u<<si);
 char *text=xkb_keymap_get_as_string(map,XKB_KEYMAP_FORMAT_TEXT_V1);if(!text)return 5;
 size_t len=strlen(text)+1;int fd=memfd_create("ncw-keymap",MFD_CLOEXEC);if(fd<0||write(fd,text,len)!=(ssize_t)len)return 5;
 struct zwp_virtual_keyboard_v1 *keyboard=zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(manager,seat);
 zwp_virtual_keyboard_v1_keymap(keyboard,WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1,fd,len);
 if(wl_display_roundtrip(d)<0)return 6;
 close(fd);free(text);xkb_keymap_unref(map);xkb_context_unref(c);
 if(chord){
  zwp_virtual_keyboard_v1_key(keyboard,now(),29,1);
  zwp_virtual_keyboard_v1_key(keyboard,now(),42,1);
  zwp_virtual_keyboard_v1_modifiers(keyboard,mods,0,0,0);
  if(wl_display_roundtrip(d)<0)return 6;
  usleep(20000);
 }
 for(int i=0;i<argc-2;i++) {
  zwp_virtual_keyboard_v1_key(keyboard,now(),keys[i],WL_KEYBOARD_KEY_STATE_PRESSED);
  if(wl_display_roundtrip(d)<0)return 6;
  if(hold)usleep((useconds_t)hold*1000);
  zwp_virtual_keyboard_v1_key(keyboard,now(),keys[i],WL_KEYBOARD_KEY_STATE_RELEASED);
  if(wl_display_roundtrip(d)<0)return 6;
  if(hold)usleep((useconds_t)hold*1000);
 }
 if(chord){
  zwp_virtual_keyboard_v1_key(keyboard,now(),42,0);
  zwp_virtual_keyboard_v1_key(keyboard,now(),29,0);
  zwp_virtual_keyboard_v1_modifiers(keyboard,0,0,0,0);
  if(wl_display_roundtrip(d)<0)return 6;
 }
 zwp_virtual_keyboard_v1_destroy(keyboard);wl_display_roundtrip(d);wl_display_disconnect(d);return 0;
}
