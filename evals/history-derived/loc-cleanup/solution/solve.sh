#!/bin/sh
set -eu

rm /app/legacy_parser.py
cat > /app/parser_api.py <<'EOF'
from modern_parser import parse_records

__all__ = ["parse_records"]
EOF

python3 -m unittest -q
