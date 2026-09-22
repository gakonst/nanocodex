export type SteeringOperation = { operation_id: string; instructions: string };
export function steeringOperation(previous: SteeringOperation | undefined, instructions: string): SteeringOperation {
  return previous?.instructions === instructions ? previous : { operation_id: crypto.randomUUID(), instructions };
}
export function activePhoneCall(status: string): boolean {
  return ["preparing", "unknown", "queued", "initiated", "ringing", "in-progress"].includes(status);
}
export function pollPhoneCalls(enabled: boolean, visibility: string): boolean {
  return enabled && visibility !== "hidden";
}

export function steerablePhoneCall(status: string): boolean {
  return ["queued", "initiated", "ringing", "in-progress"].includes(status);
}
