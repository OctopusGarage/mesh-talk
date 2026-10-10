import { expect, it, vi } from "vitest";
import { rememberSavedDownload, savedDownloadPath } from "./savedDownloads";
import { useFileAvailability } from "@/store/fileAvailability";

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

it("invalidates a cached ready status when its saved path is evicted", () => {
  useFileAvailability
    .getState()
    .merge([{ file_conv: "evicted-file", done: 1, total: 1, ready: true }]);
  rememberSavedDownload("evicted-file", "/Downloads/old.pdf");
  for (let i = 0; i < 500; i++)
    rememberSavedDownload(`new-file-${i}`, `/Downloads/${i}.pdf`);
  expect(savedDownloadPath("evicted-file")).toBeUndefined();
  expect(
    useFileAvailability.getState().statuses["evicted-file"],
  ).toBeUndefined();
});
