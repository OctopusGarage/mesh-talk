// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
import {
  useDiagnosticsController,
  type DiagnosticsController,
} from "./useDiagnosticsController";
import { useTranslation } from "react-i18next";
const native = vi.hoisted(() => ({
  invoke: vi.fn(),
  save: vi.fn(),
  reveal: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: native.save }));
vi.mock("@tauri-apps/plugin-opener", () => ({
  revealItemInDir: native.reveal,
}));
let root: Root;
let controller: DiagnosticsController;
function Probe({ open }: { open: boolean }) {
  const { t } = useTranslation();
  controller = useDiagnosticsController(open, t);
  return null;
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
async function render(open: boolean) {
  await act(async () => root.render(createElement(Probe, { open })));
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.useFakeTimers();
  vi.resetAllMocks();
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: vi.fn().mockResolvedValue(undefined) },
  });
  native.invoke.mockImplementation((command: string) =>
    Promise.resolve(command === "diag_get_peers" ? [] : null),
  );
  root = createRoot(document.createElement("div"));
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
  vi.clearAllMocks();
  vi.useRealTimers();
});
it("times out initial diagnostic reads and retries without accepting their late snapshots", async () => {
  const network = deferred<null>();
  const environment = deferred<null>();
  native.invoke.mockImplementation((command: string) =>
    command === "diag_network_info"
      ? network.promise
      : command === "env_info"
        ? environment.promise
        : Promise.resolve([]),
  );
  await render(true);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10_000);
  });
  expect(controller.networkLoading).toBe(false);
  expect(controller.environmentLoading).toBe(false);
  expect(controller.networkError).toContain("timed out");
  native.invoke.mockImplementation((command: string) =>
    Promise.resolve(
      command === "diag_network_info" ? { own_name: "fresh" } : [],
    ),
  );
  await act(async () => {
    await controller.loadNetwork();
    network.resolve(null);
    environment.resolve(null);
  });
  expect(controller.info).toEqual({ own_name: "fresh" });
});
it("does not reveal a delayed log path after the dialog closes", async () => {
  const path = deferred<string>();
  native.invoke.mockImplementation((command: string) =>
    command === "get_log_file"
      ? path.promise
      : Promise.resolve(command === "diag_get_peers" ? [] : null),
  );
  await render(true);
  await act(async () => {
    void controller.revealLogs();
  });
  await render(false);
  await act(async () => path.resolve("/tmp/log"));
  expect(native.reveal).not.toHaveBeenCalled();
});
it("does not save a delayed destination after unmount", async () => {
  const path = deferred<string>();
  native.save.mockReturnValue(path.promise);
  await render(true);
  await act(async () => {
    void controller.saveLogTail();
  });
  await act(async () => root.unmount());
  await act(async () => path.resolve("/tmp/log"));
  expect(native.invoke).not.toHaveBeenCalledWith(
    "save_log_tail",
    expect.anything(),
  );
  root = createRoot(document.createElement("div"));
});
it("suppresses old log publication, clipboard effects and errors across reopen", async () => {
  const show = deferred<string>();
  const copy = deferred<string>();
  const rescan = deferred<void>();
  native.invoke.mockImplementation((command: string) => {
    if (command === "read_log_tail") return show.promise;
    if (command === "rescan_peers") return rescan.promise;
    return Promise.resolve(command === "diag_get_peers" ? [] : null);
  });
  await render(true);
  await act(async () => {
    void controller.showLogTail();
  });
  native.invoke.mockImplementation((command: string) =>
    command === "read_log_tail" ? copy.promise : Promise.resolve([]),
  );
  await act(async () => {
    void controller.copyLogTail();
  });
  // Rescan was started in this opening as well.
  native.invoke.mockImplementation((command: string) =>
    command === "rescan_peers" ? rescan.promise : Promise.resolve([]),
  );
  await act(async () => {
    void controller.rescan();
  });
  await render(false);
  await render(true);
  await act(async () => {
    show.resolve("old");
    copy.resolve("private old logs");
    rescan.reject(new Error("old failure"));
  });
  expect(controller.logTail).toBeNull();
  expect(controller.error).toBeNull();
  expect(controller.rescanned).toBe(false);
  expect(navigator.clipboard.writeText).not.toHaveBeenCalled();
});
it("lets the latest network and environment refresh win independently", async () => {
  const network = deferred<null>();
  const environment = deferred<null>();
  native.invoke.mockImplementation((command: string) =>
    command === "diag_network_info"
      ? network.promise
      : command === "env_info"
        ? environment.promise
        : Promise.resolve([]),
  );
  await render(true);
  native.invoke.mockImplementation((command: string) =>
    command === "diag_network_info"
      ? Promise.resolve({ own_name: "new" })
      : command === "env_info"
        ? Promise.reject(new Error("environment unavailable"))
        : Promise.resolve([]),
  );
  await act(async () => {
    await Promise.all([controller.loadNetwork(), controller.loadEnvironment()]);
  });
  await act(async () => {
    network.resolve(null);
    environment.resolve(null);
  });
  expect(controller.info).toEqual({ own_name: "new" });
  expect(controller.networkError).toBeNull();
  expect(controller.environmentError).toBe("environment unavailable");
  expect(controller.networkLoading).toBe(false);
  expect(controller.environmentLoading).toBe(false);
  expect(controller.peersError).toBeNull();
});
it("keeps successful environment data when network and peer reads fail", async () => {
  native.invoke.mockImplementation((command: string) =>
    command === "env_info"
      ? Promise.resolve({ app_version: "1" })
      : Promise.reject(new Error(command)),
  );
  await render(true);
  expect(controller.env).toEqual({ app_version: "1" });
  expect(controller.networkError).toBe("diag_network_info");
  expect(controller.peersError).toBe("diag_get_peers");
  expect(controller.environmentError).toBeNull();
});
it("gives the latest rescan feedback its full lifetime and cleans timers on close", async () => {
  await render(true);
  await act(async () => {
    await controller.rescan();
  });
  await act(async () => {
    vi.advanceTimersByTime(1000);
  });
  await act(async () => {
    await controller.rescan();
  });
  await act(async () => {
    vi.advanceTimersByTime(500);
  });
  expect(controller.rescanned).toBe(true);
  await act(async () => {
    vi.advanceTimersByTime(1000);
  });
  expect(controller.rescanned).toBe(false);
  await act(async () => {
    await controller.rescan();
  });
  await render(false);
  expect(vi.getTimerCount()).toBe(0);
  await render(true);
  expect(controller.rescanned).toBe(false);
});
it("resets each opening's peer baseline while preserving infrastructure rows and bounded activity", async () => {
  const peer = {
    user_id: "relay",
    name: "Relay",
    post_office: true,
    ip: "127.0.0.1",
    tcp_port: 1,
    last_seen_secs: 0,
  };
  let peers = [peer];
  native.invoke.mockImplementation((command: string) =>
    Promise.resolve(command === "diag_get_peers" ? peers : null),
  );
  await render(true);
  expect(controller.peers).toEqual([peer]);
  expect(controller.log).toHaveLength(1);
  await render(false);
  peers = [];
  await render(true);
  expect(controller.log).toHaveLength(1);
  peers = Array.from({ length: 205 }, (_, index) => ({
    ...peer,
    user_id: String(index),
  }));
  await act(async () => {
    vi.advanceTimersByTime(1500);
  });
  expect(controller.log).toHaveLength(200);
  expect(controller.peers).toHaveLength(205);
  await act(async () => controller.clearLog());
  expect(controller.log).toEqual([]);
});
it("keeps the latest requested log preview and supports active action errors", async () => {
  await render(true);
  const old = deferred<string>();
  native.invoke.mockImplementationOnce(() => old.promise);
  await act(async () => {
    void controller.showLogTail();
  });
  native.invoke.mockResolvedValueOnce("new tail");
  await act(async () => {
    await controller.showLogTail();
  });
  await act(async () => old.resolve("old tail"));
  expect(controller.logTail).toBe("new tail");
  native.invoke.mockRejectedValueOnce(new Error("read denied"));
  await act(async () => {
    await controller.showLogTail();
  });
  expect(controller.error).toBe("read denied");
  await act(async () => controller.dismissError());
  expect(controller.error).toBeNull();
});
it("ignores pending network and environment reads from a previous opening", async () => {
  const network = deferred<unknown>();
  const environment = deferred<unknown>();
  native.invoke.mockImplementation((command: string) =>
    command === "diag_network_info"
      ? network.promise
      : command === "env_info"
        ? environment.promise
        : Promise.resolve([]),
  );
  await render(true);
  await render(false);
  native.invoke.mockImplementation((command: string) =>
    Promise.resolve(command === "diag_get_peers" ? [] : { current: true }),
  );
  await render(true);
  await act(async () => {
    network.resolve({ old: true });
    environment.reject(new Error("old error"));
  });
  expect(controller.info).toEqual({ current: true });
  expect(controller.env).toEqual({ current: true });
  expect(controller.environmentError).toBeNull();
});
it("expires acknowledged rescan feedback while a later rescan remains pending", async () => {
  await render(true);
  await act(async () => {
    await controller.rescan();
  });
  const pending = deferred<void>();
  native.invoke.mockImplementationOnce(() => pending.promise);
  await act(async () => {
    void controller.rescan();
  });
  await act(async () => {
    vi.advanceTimersByTime(1500);
  });
  expect(controller.rescanned).toBe(false);
  await act(async () => pending.resolve());
  expect(controller.rescanned).toBe(true);
});
