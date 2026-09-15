// The original public module shares the already-owned native initializer.
// Capture the callable once. Returning it directly preserves its cached Promise.
const setupCUA = globalThis.__skyreInitialize;
export { setupCUA };
