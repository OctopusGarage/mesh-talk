// @vitest-environment jsdom
import { act, createElement, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi, type Mock } from "vitest";
import "@/lib/i18n";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { useSettings } from "@/store/settings";
import { useScreenshotShortcuts } from "./useScreenshotShortcuts";
import type {
  ScreenshotShortcutEvent,
  ScreenshotRegistration,
} from "@/lib/api/screenshotShortcuts";
const native = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
const capture = vi.fn();
let root: Root;
let event: (event: { payload: ScreenshotShortcutEvent }) => void;
let unlisten: Mock<() => void>;
const status: ScreenshotRegistration = {
  global_active: true,
  error: null,
  generation: 41,
  registration_id: 8,
};
function Probe() {
  useScreenshotShortcuts({
    available: true,
    blocked: false,
    writable: true,
    capture,
  });
  return null;
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetAllMocks();
  capture.mockResolvedValue(undefined);
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
    generation: 2,
  });
  useChat.setState({ ready: true });
  useSettings.getState().setOwner(null);
  useSettings.getState().setOwner("alice");
  useSettings.getState().setScreenshotShortcuts({
    capture: "Alt+KeyB",
    capture_hidden: null,
    global: true,
  });
  unlisten = vi.fn();
  native.listen.mockImplementation(async (_name, callback) => {
    event = callback;
    return unlisten;
  });
  native.invoke.mockImplementation(async (name) =>
    name === "sync_screenshot_shortcuts" ? status : undefined,
  );
  root = createRoot(document.createElement("div"));
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
it("compares native generation with native status rather than the frontend generation", async () => {
  await act(async () => root.render(createElement(Probe)));
  await act(async () =>
    event({ payload: { owner: "alice", generation: 2, hide_window: true } }),
  );
  expect(capture).not.toHaveBeenCalled();
  await act(async () =>
    event({ payload: { owner: "alice", generation: 41, hide_window: true } }),
  );
  expect(capture).toHaveBeenCalledWith(true);
});
it("cleans up a delayed listener after unmount without registering global keys", async () => {
  let finish!: (cleanup: () => void) => void;
  native.listen.mockReturnValue(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  await act(async () => root.render(createElement(Probe)));
  await act(async () => root.render(null));
  await act(async () => finish(unlisten));
  expect(unlisten).toHaveBeenCalledOnce();
  expect(native.invoke).not.toHaveBeenCalledWith(
    "sync_screenshot_shortcuts",
    expect.anything(),
  );
});
it("suspends a registration whose sync finishes after its view was retired", async () => {
  let finish!: (value: ScreenshotRegistration) => void;
  native.invoke.mockReturnValueOnce(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  await act(async () => root.render(createElement(Probe)));
  await act(async () => root.render(null));
  await act(async () => finish(status));
  expect(native.invoke).toHaveBeenCalledWith("suspend_screenshot_shortcuts", {
    owner: "alice",
    generation: 41,
    registrationId: 8,
  });
});
it("does not let StrictMode's abandoned listener start a second native registration", async () => {
  await act(async () =>
    root.render(createElement(StrictMode, null, createElement(Probe))),
  );
  expect(native.listen).toHaveBeenCalledTimes(2);
  expect(
    native.invoke.mock.calls.filter(
      ([name]) => name === "sync_screenshot_shortcuts",
    ),
  ).toHaveLength(1);
});

it("renews its lease when runtime identity changes while chat remains ready", async () => {
  await act(async () => root.render(createElement(Probe)));
  await act(async () =>
    useChat.setState((state) => ({ identityEpoch: state.identityEpoch + 1 })),
  );
  await act(async () =>
    event({ payload: { owner: "alice", generation: 41, hide_window: true } }),
  );
  expect(capture).toHaveBeenCalledWith(true);
});
