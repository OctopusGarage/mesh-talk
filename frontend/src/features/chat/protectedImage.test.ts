// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { createProtectedImageUrl } from "./protectedImage";
afterEach(() => vi.unstubAllGlobals());
it("preserves MIME and waits for decoding before returning a local URL", async () => {
  let decode!: () => void;
  const ready = new Promise<void>((yes) => {
    decode = yes;
  });
  const create = vi.fn<(blob: Blob) => string>(() => "blob:protected");
  const revoke = vi.fn();
  vi.stubGlobal("URL", { createObjectURL: create, revokeObjectURL: revoke });
  vi.stubGlobal(
    "Image",
    class {
      src = "";
      decode() {
        return ready;
      }
    },
  );
  const result = createProtectedImageUrl(new ArrayBuffer(4), "image/webp");
  expect(create.mock.calls[0][0].type).toBe("image/webp");
  decode();
  expect(await result).toBe("blob:protected");
  expect(revoke).not.toHaveBeenCalled();
});
it("revokes failed image decoding", async () => {
  const revoke = vi.fn();
  vi.stubGlobal("URL", {
    createObjectURL: () => "blob:broken",
    revokeObjectURL: revoke,
  });
  vi.stubGlobal(
    "Image",
    class {
      src = "";
      decode() {
        return Promise.reject(new Error("bad image"));
      }
    },
  );
  await expect(
    createProtectedImageUrl(new ArrayBuffer(1), "image/png"),
  ).rejects.toThrow("bad image");
  expect(revoke).toHaveBeenCalledWith("blob:broken");
});
it("immediately revokes an image URL when its reveal is cancelled during decoding", async () => {
  const revoke = vi.fn();
  vi.stubGlobal("URL", {
    createObjectURL: () => "blob:pending",
    revokeObjectURL: revoke,
  });
  vi.stubGlobal(
    "Image",
    class {
      src = "";
      decode() {
        return new Promise<void>(() => {});
      }
    },
  );
  const abort = new AbortController();
  const pending = createProtectedImageUrl(
    new ArrayBuffer(1),
    "image/png",
    abort.signal,
  );
  abort.abort();
  expect(revoke).toHaveBeenCalledExactlyOnceWith("blob:pending");
  await expect(pending).rejects.toThrow("cancelled");
});
