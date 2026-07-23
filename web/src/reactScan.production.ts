// Production builds alias react-scan to this no-op module so profiling adds
// no runtime code or startup work to the embedded demo.
export function scan(_options?: unknown): void {}
