#!/usr/bin/env bash

url='https://openai.com'

printf 'OSC 8 link: \033]8;;%s\033\\OpenAI (click me)\033]8;;\033\\\n' "$url"
printf 'Plain URL:  %s\n' "$url"
printf '\nInside tmux/Codex, hold Command+Shift while clicking.\n'
