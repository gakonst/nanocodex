#!/bin/sh
set -eu

cat > /app/accumulator.mjs <<'EOF'
export function createState() {
  return { generation: 0, files: [], pending: null };
}

function appendChunk(files, event) {
  const index = files.findIndex((file) => file.path === event.path);
  if (index === -1) {
    return [...files, { path: event.path, sequence: event.sequence, text: event.text }];
  }

  const current = files[index];
  if (event.sequence <= current.sequence) return files;

  const next = files.slice();
  next[index] = {
    ...current,
    sequence: event.sequence,
    text: current.text + event.text,
  };
  return next;
}

export function applyEvent(state, event) {
  if (event.type === "snapshot_start") {
    return {
      ...state,
      pending: { generation: event.generation, files: [] },
    };
  }

  if (event.type === "snapshot_commit") {
    if (!state.pending || state.pending.generation !== event.generation) return state;
    return {
      generation: event.generation,
      files: state.pending.files,
      pending: null,
    };
  }

  if (event.type !== "file_chunk") return state;

  if (state.pending) {
    const files = appendChunk(state.pending.files, event);
    if (files === state.pending.files) return state;
    return { ...state, pending: { ...state.pending, files } };
  }

  const files = appendChunk(state.files, event);
  return files === state.files ? state : { ...state, files };
}
EOF

node /app/test.mjs
