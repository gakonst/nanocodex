import { describe, expect, it, vi } from "vitest";

import {
  withAiSearchItem,
  withAiSearchItems,
  withAiSearchResult,
} from "../src/memory-scope";

describe("AI Search item RPC ownership", () => {
  it("disposes the items collection after a successful operation", async () => {
    const dispose = vi.fn();
    const items = {
      delete: vi.fn(async () => {}),
      [Symbol.dispose]: dispose,
    };
    const instance = { items } as unknown as Pick<AiSearchInstance, "items">;

    await expect(withAiSearchItems(instance, async (current) => {
      await current.delete("item-0");
      return "done";
    })).resolves.toBe("done");
    expect(items.delete).toHaveBeenCalledExactlyOnceWith("item-0");
    expect(dispose).toHaveBeenCalledOnce();
  });

  it("disposes the items collection when the operation fails", async () => {
    const dispose = vi.fn();
    const items = { [Symbol.dispose]: dispose };
    const instance = { items } as unknown as Pick<AiSearchInstance, "items">;

    await expect(withAiSearchItems(instance, async () => {
      throw new Error("AI Search unavailable");
    })).rejects.toThrow("AI Search unavailable");
    expect(dispose).toHaveBeenCalledOnce();
  });

  it("supports the local AI Search collection without a disposer", async () => {
    const items = {
      delete: vi.fn(async () => {}),
    };
    const instance = { items } as unknown as Pick<AiSearchInstance, "items">;

    await expect(withAiSearchItems(instance, async (current) => {
      await current.delete("item-local");
    })).resolves.toBeUndefined();
    expect(items.delete).toHaveBeenCalledExactlyOnceWith("item-local");
  });

  it("disposes an object-valued RPC result after consuming it", async () => {
    const dispose = vi.fn();
    const result = {
      id: "item-result",
      status: "completed",
      [Symbol.dispose]: dispose,
    };

    await expect(withAiSearchResult(Promise.resolve(result), (value) => value.id))
      .resolves.toBe("item-result");
    expect(dispose).toHaveBeenCalledOnce();
  });

  it("disposes an object-valued RPC result when its consumer fails", async () => {
    const dispose = vi.fn();
    const result = { [Symbol.dispose]: dispose };

    await expect(withAiSearchResult(Promise.resolve(result), () => {
      throw new Error("invalid AI Search response");
    })).rejects.toThrow("invalid AI Search response");
    expect(dispose).toHaveBeenCalledOnce();
  });

  it("disposes the retained item stub after a successful operation", async () => {
    const dispose = vi.fn();
    const info = Object.freeze({ id: "item-1", status: "completed" });
    const item = {
      info: vi.fn(async () => info),
      [Symbol.dispose]: dispose,
    };
    const items = {
      get: vi.fn(() => item),
    } as unknown as Pick<AiSearchItems, "get">;

    await expect(withAiSearchItem(items, "item-1", (current) => current.info()))
      .resolves.toBe(info);
    expect(items.get).toHaveBeenCalledExactlyOnceWith("item-1");
    expect(dispose).toHaveBeenCalledOnce();
  });

  it("disposes the retained item stub when the operation fails", async () => {
    const dispose = vi.fn();
    const item = {
      info: vi.fn(async () => { throw new Error("AI Search unavailable"); }),
      [Symbol.dispose]: dispose,
    };
    const items = {
      get: vi.fn(() => item),
    } as unknown as Pick<AiSearchItems, "get">;

    await expect(withAiSearchItem(items, "item-2", (current) => current.info()))
      .rejects.toThrow("AI Search unavailable");
    expect(dispose).toHaveBeenCalledOnce();
  });

  it("disposes the retained item stub after requesting a resync", async () => {
    const dispose = vi.fn();
    const synced = Object.freeze({ id: "item-3", status: "pending" });
    const item = {
      sync: vi.fn(async () => synced),
      [Symbol.dispose]: dispose,
    };
    const items = {
      get: vi.fn(() => item),
    } as unknown as Pick<AiSearchItems, "get">;

    await expect(withAiSearchItem(items, "item-3", (current) => current.sync()))
      .resolves.toBe(synced);
    expect(item.sync).toHaveBeenCalledOnce();
    expect(dispose).toHaveBeenCalledOnce();
  });

  it("disposes collection, item, and result ownership layers", async () => {
    const itemsDispose = vi.fn();
    const itemDispose = vi.fn();
    const resultDispose = vi.fn();
    const result = {
      id: "item-4",
      status: "completed" as const,
      [Symbol.dispose]: resultDispose,
    };
    const item = {
      info: vi.fn(async () => result),
      [Symbol.dispose]: itemDispose,
    };
    const items = {
      get: vi.fn(() => item),
      [Symbol.dispose]: itemsDispose,
    };
    const instance = { items } as unknown as Pick<AiSearchInstance, "items">;

    await expect(withAiSearchItems(
      instance,
      (currentItems) => withAiSearchItem(
        currentItems,
        "item-4",
        (currentItem) => withAiSearchResult(
          currentItem.info(),
          (info) => ({ id: info.id, status: info.status }),
        ),
      ),
    )).resolves.toEqual({ id: "item-4", status: "completed" });
    expect(itemsDispose).toHaveBeenCalledOnce();
    expect(itemDispose).toHaveBeenCalledOnce();
    expect(resultDispose).toHaveBeenCalledOnce();
  });
});
