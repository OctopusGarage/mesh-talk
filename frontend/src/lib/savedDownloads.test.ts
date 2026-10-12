import { expect, it, vi } from "vitest";
import { rememberSavedDownload, savedDownloadPath } from "./savedDownloads";
import { useFileAvailability } from "@/store/fileAvailability";
import { useAuth } from "@/store/auth";

it("remembers the exact destination of a saved file", () => {
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
  });
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => store.set(key, value),
  });
  rememberSavedDownload("file-1", "/Downloads/report (1).pdf");
  expect(savedDownloadPath("file-1")).toBe("/Downloads/report (1).pdf");
  expect(savedDownloadPath("other-file")).toBeUndefined();
  expect(JSON.parse(store.get("mesh-talk-downloads:alice")!)["file-1"]).toBe(
    "/Downloads/report (1).pdf",
  );
  vi.unstubAllGlobals();
});

it("invalidates a cached ready status when its saved path is evicted", () => {
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
  });
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

it("keeps saved locations private to the signed-in account", () => {
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => store.set(key, value),
  });
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
  });
  rememberSavedDownload("same-file", "/Alice/private.pdf");
  useAuth.setState({
    user: { id: "bob", username: "bob", display_name: "Bob" },
  });
  expect(savedDownloadPath("same-file")).toBeUndefined();
  rememberSavedDownload("same-file", "/Bob/private.pdf");
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
  });
  expect(savedDownloadPath("same-file")).toBe("/Alice/private.pdf");
  vi.unstubAllGlobals();
});

it("archives unowned download locations instead of showing them to a new login", () => {
  const store = new Map<string, string>([
    ["mesh-talk-downloads", '{"file":"/old/private"}'],
  ]);
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => store.set(key, value),
    removeItem: (key: string) => store.delete(key),
  });
  useAuth.setState({
    user: { id: "new", username: "new", display_name: "New" },
  });
  expect(savedDownloadPath("file")).toBeUndefined();
  expect(store.has("mesh-talk-downloads")).toBe(false);
  expect(store.get("mesh-talk-downloads:legacy-unassigned")).toBe(
    '{"file":"/old/private"}',
  );
  vi.unstubAllGlobals();
});
