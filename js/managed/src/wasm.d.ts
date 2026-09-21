declare module "*.wasm" {
  const module: WebAssembly.Module;
  export default module;
}
declare module '*.txt' {
  const text: string;
  export default text;
}
declare module '*.bin' {
  const data: ArrayBuffer;
  export default data;
}
