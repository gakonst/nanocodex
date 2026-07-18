#!/bin/sh
set -eu

git -C /app show goal~1:client.py > /app/client.py
python3 -m unittest -q
