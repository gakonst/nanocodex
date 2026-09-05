import { describe, expect, it } from "vitest";

import {
  MAX_ROOM_MESSAGE_BYTES,
  truncateRoomMessage,
} from "../src/multiplayer-protocol";

describe("Multiplayer public message bounds", () => {
  it("keeps projected model replies valid at a UTF-8 boundary", () => {
    expect(truncateRoomMessage("hello")).toBe("hello");
    const projected = truncateRoomMessage(`answer:${"🙂".repeat(MAX_ROOM_MESSAGE_BYTES)}`);
    expect(projected.endsWith("…")).toBe(true);
    expect(new TextEncoder().encode(projected).byteLength).toBeLessThanOrEqual(
      MAX_ROOM_MESSAGE_BYTES,
    );
    expect(projected).not.toContain("�");
  });
});
