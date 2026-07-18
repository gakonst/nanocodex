FROM python:3.13-bookworm

WORKDIR /app
COPY README.md legacy_parser.py modern_parser.py parser_api.py test_parser.py ./

RUN python3 -m unittest -q
