// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const state = vi.hoisted(() => ({
  owner: "alice",
  generation: 1,
  locked: false,
  phase: "idle",
  open: vi.fn(),
  stop: vi.fn(),
  loopback: vi.fn(),
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/store/auth", () => ({
  useAuth: (select: (s: unknown) => unknown) =>
    select({ user: { id: state.owner }, generation: state.generation }),
}));
vi.mock("@/store/chat", () => ({
  useChat: (select: (s: unknown) => unknown) =>
    select({ runEpoch: 1, identityEpoch: 1 }),
}));
vi.mock("@/store/appLock", () => ({
  useAppLock: (select: (s: unknown) => unknown) =>
    select({ locked: state.locked }),
}));
vi.mock("@/store/calls", () => ({
  useCalls: (select: (s: unknown) => unknown) => select({ phase: state.phase }),
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ current: () => true }),
}));
vi.mock("@/lib/mediaDeviceCheck", () => ({
  openMediaDeviceCheck: state.open,
  MediaDeviceCheckError: class extends Error {
    code = "permission";
  },
}));
import { MediaDeviceCheck } from "./MediaDeviceCheck";
let host: HTMLDivElement, root: Root;
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  state.owner = "alice";
  state.generation = 1;
  state.locked = false;
  state.phase = "idle";
  state.stop.mockReset();
  state.loopback.mockReset().mockResolvedValue({ bytesReceived: 20 });
  state.open.mockReset().mockResolvedValue({
    stream: {},
    stop: state.stop,
    runLoopback: state.loopback,
  });
  vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue();
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => root.render(createElement(MediaDeviceCheck)));
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
async function click(id: string) {
  await act(async () =>
    host.querySelector<HTMLButtonElement>(`[data-testid="${id}"]`)!.click(),
  );
}
it("never captures on mount and requests audio/video independently", async () => {
  expect(state.open).not.toHaveBeenCalled();
  await click("media-check-audio");
  expect(state.open.mock.calls[0][0]).toBe("audio");
  await click("media-check-video");
  expect(state.stop).toHaveBeenCalledOnce();
  expect(state.open.mock.calls[1][0]).toBe("video");
  await click("media-check-stop");
  expect(state.stop).toHaveBeenCalledTimes(2);
});
it.each(["lock", "owner", "call", "unmount"])(
  "aborts pending capture on %s",
  async (boundary) => {
    let signal!: AbortSignal;
    state.open.mockImplementation((_kind: string, value: AbortSignal) => {
      signal = value;
      return new Promise(() => {});
    });
    await click("media-check-audio");
    if (boundary === "lock") state.locked = true;
    if (boundary === "owner") state.generation++;
    if (boundary === "call") state.phase = "connected";
    await act(async () => {
      if (boundary === "unmount") root.render(null);
      else root.render(createElement(MediaDeviceCheck));
    });
    expect(signal.aborted).toBe(true);
  },
);
it("cleans a late capture and does not publish into a new run", async () => {
  let resolve!: (value: unknown) => void;
  state.open.mockReturnValueOnce(
    new Promise((done) => {
      resolve = done;
    }),
  );
  await click("media-check-audio");
  await click("media-check-stop");
  await act(async () => resolve({ stream: {}, stop: state.stop }));
  expect(state.stop).toHaveBeenCalledOnce();
  expect(host.textContent).not.toContain("mediaCheck.audioReady");
});
it("runs advanced diagnostics only explicitly and reports engine evidence", async () => {
  await click("media-check-video");
  expect(state.loopback).not.toHaveBeenCalled();
  await click("media-check-loopback");
  expect(state.loopback).toHaveBeenCalledOnce();
  expect(host.textContent).toContain("mediaCheck.loopbackPassed");
});

it("keeps microphone testing available after a camera failure", async () => {
  const { MediaDeviceCheckError } = await import("@/lib/mediaDeviceCheck");
  state.open.mockRejectedValueOnce(new MediaDeviceCheckError("permission"));
  await click("media-check-video");
  expect(host.querySelector('[role="alert"]')?.textContent).toBe(
    "mediaCheck.errors.permission",
  );
  expect(
    host.querySelector<HTMLButtonElement>('[data-testid="media-check-audio"]')
      ?.disabled,
  ).toBe(false);
  await click("media-check-audio");
  expect(state.open.mock.calls[1][0]).toBe("audio");
  expect(host.querySelector('[role="alert"]')).toBeNull();
});

it("closes an AudioContext when meter setup fails while keeping microphone capture usable", async () => {
  const close = vi.fn().mockResolvedValue(undefined);
  const disconnect = vi.fn();
  vi.stubGlobal(
    "AudioContext",
    class {
      close = close;
      createMediaStreamSource = () => ({ disconnect });
      createAnalyser = () => {
        throw new Error("Analysis unavailable");
      };
    },
  );
  await click("media-check-audio");
  expect(close).toHaveBeenCalledOnce();
  expect(disconnect).toHaveBeenCalledOnce();
  expect(state.stop).not.toHaveBeenCalled();
  expect(
    host.querySelector('[data-testid="media-check-status"]')?.textContent,
  ).toBe("mediaCheck.audioMeterUnavailable");
  expect(host.querySelector('[role="alert"]')).toBeNull();
  await act(async () => root.render(null));
  expect(state.stop).toHaveBeenCalledOnce();
  expect(close).toHaveBeenCalledOnce();
});
