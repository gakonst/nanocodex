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
 const char *command=NULL;
 if(argc!=2)return 2;
 if(!strcmp(argv[1],"prepare-session"))command="/nc bridge 953301 chord";
 else if(!strcmp(argv[1],"prepare-prompt"))command="/nc ask say hello from inside wow in one short sentence";
 else if(!strcmp(argv[1],"prepare-debug"))command="/nc bridge-debug";
 else if(strcmp(argv[1],"submit"))return 2;
 const char *letters="abcdefghijklmnopqrstuvwxyz";
 const unsigned letter_codes[]={30,48,46,32,18,33,34,35,23,36,37,38,50,49,24,25,16,19,31,20,22,47,17,45,21,44};
 unsigned keys[128];int count=0;keys[count++]=28;
 if(command)for(const char *p=command;*p;p++){
  const char *q=strchr(letters,*p);
  if(q)keys[count++]=letter_codes[q-letters];
  else if(*p==' ')keys[count++]=57;
  else if(*p=='/')keys[count++]=53;
  else if(*p=='-')keys[count++]=12;
  else if(*p>='1'&&*p<='9')keys[count++]=2+*p-'1';
  else if(*p=='0')keys[count++]=11;
  else return 2;
 }
 int chord=0;long hold=8;

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
 for(int i=0;i<count;i++) {
  zwp_virtual_keyboard_v1_key(keyboard,now(),keys[i],WL_KEYBOARD_KEY_STATE_PRESSED);
  if(wl_display_roundtrip(d)<0)return 6;
  if(hold)usleep((useconds_t)hold*1000);
  zwp_virtual_keyboard_v1_key(keyboard,now(),keys[i],WL_KEYBOARD_KEY_STATE_RELEASED);
  if(wl_display_roundtrip(d)<0)return 6;
  if(hold)usleep((useconds_t)hold*1000);
  if(i==0 && command)usleep(100000);
 }
 if(chord){
  zwp_virtual_keyboard_v1_key(keyboard,now(),42,0);
  zwp_virtual_keyboard_v1_key(keyboard,now(),29,0);
  zwp_virtual_keyboard_v1_modifiers(keyboard,0,0,0,0);
  if(wl_display_roundtrip(d)<0)return 6;
 }
 zwp_virtual_keyboard_v1_destroy(keyboard);wl_display_roundtrip(d);wl_display_disconnect(d);return 0;
}
