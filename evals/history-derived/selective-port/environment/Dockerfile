FROM python:3.13-bookworm

WORKDIR /app
COPY README.md test_client.py client.main.py ./
COPY client.goal-retry.py client.goal-output.py /tmp/fixture/

RUN cp client.main.py client.py \
    && rm client.main.py \
    && git init -q -b main \
    && git config user.name "Harbor Fixture" \
    && git config user.email "fixture@example.invalid" \
    && git add . \
    && git commit -q -m "add compatible client" \
    && git switch -q -c goal \
    && cp /tmp/fixture/client.goal-retry.py client.py \
    && git add client.py \
    && git commit -q -m "retry transient sends with backoff" \
    && cp /tmp/fixture/client.goal-output.py client.py \
    && git add client.py \
    && git commit -q -m "emit structured output" \
    && git switch -q main \
    && rm -rf /tmp/fixture \
    && python3 -m unittest -q
