#ifndef NANOCODEX_VOICE_CORE_H
#define NANOCODEX_VOICE_CORE_H
#include <stddef.h>
#include <stdint.h>
uint64_t nc_voice_create(const uint8_t *bytes, size_t length);
char *nc_voice_apply(uint64_t handle, const uint8_t *bytes, size_t length);
void nc_voice_destroy(uint64_t handle);
void nc_voice_string_free(char *value);
#endif
