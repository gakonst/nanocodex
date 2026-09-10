/** The host owns VM recipes and credentials. Remote callers choose a stable
 * name, never executable paths or environment variables. */
export function createVmTools({ hostName, list, start, stop }) {
  const parameters = {
    type: "object", properties: { name: { type: "string", description: "Stable lowercase VM name, 1–40 letters, numbers or hyphens." } },
    required: ["name"], additionalProperties: false,
  };
  const name = input => {
    if (!input || Object.keys(input).some(key => key !== "name") || typeof input.name !== "string" || !/^[a-z0-9][a-z0-9-]{0,39}$/.test(input.name)) throw new Error("Choose a VM name with 1–40 lowercase letters, numbers or hyphens.");
    return input.name;
  };
  return [
    { name: "list_vms", description: `List private Linux VMs on ${hostName}, including stopped VMs.`,
      parameters: { type: "object", properties: {}, required: [], additionalProperties: false }, handler: () => list() },
    { name: "start_vm", description: `Create or restart a private Linux VM on ${hostName}. Reuse its stable name to preserve files. Returns only when its Hand is connected, including a viewable screen when the image has a desktop; use the returned machine ID for commands. Up to four VMs can run on this host.`,
      parameters, supportsParallelToolCalls: false, handler: (input, context) => start(name(input), context?.signal) },
    { name: "stop_vm", description: `Stop a private VM created through this Hand on ${hostName}. Its files are retained for start_vm.`,
      parameters, supportsParallelToolCalls: false, handler: input => stop(name(input)) },
  ];
}

export function supportsLocalVms(platform = process.platform, arch = process.arch) {
  return (platform === "darwin" && arch === "arm64") || platform === "linux";
}
