FROM python:3.13-bookworm

WORKDIR /app
COPY README.md analyze.py test_analyze.py ./

RUN python3 -m unittest -q
