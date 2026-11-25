// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { GroupVoiceControls, GroupVoiceEntry } from "./GroupVoiceControls";
const f = vi.hoisted(() => ({
  state: {
    active: null as null | { joining: boolean; host: boolean },
    invites: {} as Record<string, { expires: number }>,
    audio: {} as Record<string, MediaStream>,
    muted: false,
    error: null as null | string,
  },
  enabled: true,
  phase: "idle",
  start: vi.fn(),
  join: vi.fn(),
  mute: vi.fn(),
  stop: vi.fn(),
  set: vi.fn(),
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/store/groupVoice", () => ({
  useGroupVoice: Object.assign(
    (select?: (state: typeof f.state) => unknown) =>
      select ? select(f.state) : f.state,
    { setState: f.set },
  ),
  startGroupVoice: f.start,
  joinGroupVoice: f.join,
  muteGroupVoice: f.mute,
  stopGroupVoice: f.stop,
}));
vi.mock("@/store/settings", () => ({
  useSettings: (select: (state: { callsEnabled: boolean }) => unknown) =>
    select({ callsEnabled: f.enabled }),
}));
vi.mock("@/store/calls", () => ({
  useCalls: (select: (state: { phase: string }) => unknown) =>
    select({ phase: f.phase }),
}));
const host = document.createElement("div");
let root = createRoot(host);
afterEach(async () => {
  await act(async () => root.unmount());
  root = createRoot(host);
  f.state = { active: null, invites: {}, audio: {}, muted: false, error: null };
  f.enabled = true;
  f.phase = "idle";
  vi.clearAllMocks();
  vi.restoreAllMocks();
});
const click = async (label: string) =>
  act(async () =>
    [...host.querySelectorAll("button")]
      .find(
        (b) =>
          b.textContent === label || b.getAttribute("aria-label") === label,
      )!
      .click(),
  );
it("starts and joins invitations without resolving members for joins", async () => {
  const members = vi.fn(() => ["peer"]);
  const render = () =>
    act(async () =>
      root.render(
        createElement(GroupVoiceEntry, {
          group: "g",
          self: "me",
          owner: "owner",
          memberDevices: members,
        }),
      ),
    );
  await render();
  await click("groupVoice.start");
  expect(f.start).toHaveBeenCalledWith("g", "me", "owner", ["peer"]);
  const invite = { expires: Date.now() + 10000 };
  f.state.invites.g = invite;
  await render();
  await click("groupVoice.join");
  expect(f.join).toHaveBeenCalledWith(invite, "me", "owner");
  expect(members).toHaveBeenCalledOnce();
  for (const conflict of ["disabled", "direct", "active"]) {
    f.enabled = conflict !== "disabled";
    f.phase = conflict === "direct" ? "active" : "idle";
    f.state.active =
      conflict === "active" ? { joining: false, host: false } : null;
    await render();
    expect(host.querySelector("button")).toBeNull();
  }
});
it("offers host mute, leave, end and error dismissal", async () => {
  f.state.active = { joining: true, host: true };
  f.state.muted = true;
  f.state.error = "groupVoice.failed";
  await act(async () => root.render(createElement(GroupVoiceControls)));
  expect(host.querySelector('[role="status"]')?.textContent).toBe(
    "groupVoice.joining",
  );
  await click("groupVoice.unmute");
  await click("groupVoice.leave");
  await click("groupVoice.end");
  await click("groupVoice.dismiss");
  expect(f.mute).toHaveBeenCalledOnce();
  expect(f.stop).toHaveBeenNthCalledWith(1, true);
  expect(f.stop).toHaveBeenNthCalledWith(2, true, true);
  expect(f.set).toHaveBeenCalledWith({ error: null });
});
it("retries autoplay rejection and detaches the remote stream", async () => {
  const stream = {} as MediaStream;
  f.state.active = { joining: false, host: false };
  f.state.audio.peer = stream;
  const play = vi
    .spyOn(HTMLMediaElement.prototype, "play")
    .mockRejectedValueOnce(new Error("blocked"))
    .mockResolvedValue(undefined);
  const pause = vi
    .spyOn(HTMLMediaElement.prototype, "pause")
    .mockImplementation(() => {});
  await act(async () => root.render(createElement(GroupVoiceControls)));
  const audio = host.querySelector("audio")!;
  expect(audio.srcObject).toBe(stream);
  await click("groupVoice.play");
  expect(play).toHaveBeenCalledTimes(2);
  expect(host.textContent).not.toContain("groupVoice.play");
  expect(host.textContent).not.toContain("groupVoice.end");
  await act(async () => root.unmount());
  root = createRoot(host);
  expect(pause).toHaveBeenCalledOnce();
  expect(audio.srcObject).toBeNull();
});
it("hides an idle control panel", async () => {
  await act(async () => root.render(createElement(GroupVoiceControls)));
  expect(host.childElementCount).toBe(0);
});

it("keeps playback recovery available after another rejection", async () => {
  f.state.active = { joining: false, host: false };
  f.state.audio.peer = {} as MediaStream;
  const play = vi
    .spyOn(HTMLMediaElement.prototype, "play")
    .mockRejectedValue(new Error("blocked"));
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
  await act(async () => root.render(createElement(GroupVoiceControls)));
  await click("groupVoice.play");
  expect(host.textContent).toContain("groupVoice.play");
  play.mockResolvedValue(undefined);
  await click("groupVoice.play");
  expect(host.textContent).not.toContain("groupVoice.play");
});
it("ignores playback completion after remote audio is removed", async () => {
  let complete!: () => void;
  f.state.active = { joining: false, host: false };
  f.state.audio.peer = {} as MediaStream;
  vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(
    () =>
      new Promise<void>((resolve) => {
        complete = resolve;
      }),
  );
  const pause = vi
    .spyOn(HTMLMediaElement.prototype, "pause")
    .mockImplementation(() => {});
  await act(async () => root.render(createElement(GroupVoiceControls)));
  f.state.audio = {};
  await act(async () => root.render(createElement(GroupVoiceControls)));
  await act(async () => complete());
  expect(pause).toHaveBeenCalledOnce();
  expect(host.querySelector("audio")).toBeNull();
  expect(host.textContent).not.toContain("groupVoice.play");
});
