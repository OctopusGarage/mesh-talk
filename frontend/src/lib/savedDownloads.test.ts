import { expect, it, vi } from "vitest";
import { rememberSavedDownload, savedDownloadPath } from "./savedDownloads";

it("remembers the exact destination of a saved file", () => {
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => store.set(key, value),
  });
  rememberSavedDownload("file-1", "/Downloads/report (1).pdf");
  expect(savedDownloadPath("file-1")).toBe("/Downloads/report (1).pdf");
  expect(savedDownloadPath("other-file")).toBeUndefined();
  expect(JSON.parse(store.get("mesh-talk-downloads")!)["file-1"]).toBe(
    "/Downloads/report (1).pdf",
  );
  vi.unstubAllGlobals();
});
