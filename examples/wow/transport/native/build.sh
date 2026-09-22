#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")"
wayland-scanner client-header virtual-keyboard.xml virtual-keyboard.h
wayland-scanner private-code virtual-keyboard.xml virtual-keyboard.c
cc -O2 -Wall -Wextra -Werror carrier-keys.c virtual-keyboard.c -o carrier-keys $(pkg-config --cflags --libs wayland-client xkbcommon)
