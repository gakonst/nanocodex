export type DeliveryRecord = Readonly<{
  status: "completed";
}> | Readonly<{
  expiresAt: number;
  status: "claimed";
  token: string;
}>;

export type DeliveryClaim = Readonly<{
  record: DeliveryRecord;
  status: "claimed" | "completed" | "in_progress";
  token?: string;
}>;

export function claimDelivery(
  retained: DeliveryRecord | undefined,
  now: number,
  expiresAt: number,
  token: string,
): DeliveryClaim {
  if (retained?.status === "completed") return { record: retained, status: "completed" };
  if (retained?.status === "claimed" && retained.expiresAt > now) {
    return { record: retained, status: "in_progress" };
  }
  const record = { expiresAt, status: "claimed", token } as const;
  return { record, status: "claimed", token };
}

export function completeDelivery(
  retained: DeliveryRecord | undefined,
  token: string,
): DeliveryRecord | undefined {
  if (retained?.status === "completed") return retained;
  return retained?.status === "claimed" && retained.token === token
    ? { status: "completed" }
    : undefined;
}

export function releaseDelivery(
  retained: DeliveryRecord | undefined,
  token: string,
): boolean {
  return retained?.status === "claimed" && retained.token === token;
}
