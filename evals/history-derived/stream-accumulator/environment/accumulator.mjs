export function createState() {
  return { generation: 0, files: [], pending: null };
}

export function applyEvent(state, event) {
  if (event.type === "snapshot_start") {
    return {
      generation: state.generation,
      files: [],
      pending: { generation: event.generation, files: [] },
    };
  }
  if (event.type === "snapshot_commit") {
    return {
      generation: event.generation,
      files: state.pending?.files ?? [],
      pending: null,
    };
  }
  if (event.type !== "file_chunk") return state;

  const files = state.files.map((file) => ({ ...file }));
  let file = files.find((candidate) => candidate.path === event.path);
  if (!file) {
    file = { path: event.path, sequence: -1, text: "" };
    files.push(file);
  }
  file.sequence = event.sequence;
  file.text += event.text;
  return { ...state, files };
}
