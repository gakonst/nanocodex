FROM python:3.12-slim

COPY arc_agi_3.py /usr/local/bin/arc-agi-3
COPY arc_case.json /opt/nanocodex/arc-agi-3/case.json
RUN chmod 0755 /usr/local/bin/arc-agi-3

WORKDIR /workspace
