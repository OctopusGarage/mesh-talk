// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useChat } from "@/store/chat";
import { useAuth } from "@/store/auth";
import type { PeerInfo } from "@/lib/types";
import { ChatApp } from "./ChatApp";

const mocks = vi.hoisted(() => ({ shell: vi.fn(), invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("./Sidebar", () => ({
  Sidebar: () => {
    mocks.shell();
    return null;
  },
}));
vi.mock("./ConversationView", () => ({ ConversationView: () => null }));
vi.mock("./CallDialog", () => ({ CallDialog: () => null }));
vi.mock("./AccountProtectionNotice", () => ({
  AccountProtectionNotice: () => null,
}));
vi.mock("./DeliveryRecoveryProvider", () => ({
  DeliveryRecoveryProvider: ({ children }: { children: ReactNode }) => children,
}));
vi.mock("@/lib/motion", () => ({ useMotionOK: () => false, ease: "linear" }));

let root: Root;
let host: HTMLDivElement;
function peer(id: string, account = "account-a"): PeerInfo {
  return {
    user_id: id,
    account_id: account,
    name: "Fixture",
    addr: "127.0.0.1:1",
    post_office: false,
  };
}
function contexts() {
  return mocks.invoke.mock.calls
    .filter(([command]) => command === "set_notification_context")
    .map(([, args]) => args);
}
async function mount() {
  await act(async () => root.render(createElement(ChatApp)));
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.invoke.mockReset().mockResolvedValue(undefined);
  mocks.shell.mockClear();
  useAuth.setState({ user: null, generation: 1 });
  useChat.setState({
    ready: false,
    error: null,
    active: { kind: "channel", id: "channel-a", name: "Channel A" },
    peers: [peer("device-a")],
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

it("keeps the channel context and shell quiet through heartbeat and identical selection replacements", async () => {
  await mount();
  const calls = contexts().length;
  const renders = mocks.shell.mock.calls.length;
  for (let tick = 0; tick < 3; tick++) {
    await act(async () =>
      useChat.setState({
        peers: [{ ...peer("device-a"), name: `Lease ${tick}` }],
        active: { kind: "channel", id: "channel-a", name: "Channel A" },
      }),
    );
  }
  expect(contexts()).toHaveLength(calls);
  expect(mocks.shell).toHaveBeenCalledTimes(renders);
});

it("ignores account peer order, names and unrelated devices but publishes matching membership changes", async () => {
  useChat.setState({
    active: { kind: "account", id: "account-a", name: "Account A" },
    peers: [peer("device-b"), peer("device-a")],
  });
  await mount();
  const calls = contexts().length;
  const renders = mocks.shell.mock.calls.length;
  await act(async () =>
    useChat.setState({
      active: { kind: "account", id: "account-a", name: "Account A" },
      peers: [
        { ...peer("device-a"), name: "Renamed" },
        peer("device-z", "other"),
        peer("device-b"),
      ],
    }),
  );
  expect(contexts()).toHaveLength(calls);
  expect(mocks.shell).toHaveBeenCalledTimes(renders);
  await act(async () =>
    useChat.setState({ peers: [peer("device-c"), peer("device-a")] }),
  );
  expect(contexts()[contexts().length - 1]).toMatchObject({
    activePeerDevices: ["device-a", "device-c"],
    activeChannel: null,
  });
  expect(contexts()).toHaveLength(calls + 2);
  await act(async () => useChat.setState({ peers: [] }));
  expect(contexts()[contexts().length - 1]).toMatchObject({
    activePeerDevices: [],
    activeChannel: null,
  });
  expect(contexts()).toHaveLength(calls + 4);
});

it("publishes conversation changes and clears on generation replacement and unmount with increasing revisions", async () => {
  await mount();
  await act(async () =>
    useChat.setState({
      active: { kind: "channel", id: "channel-b", name: "Channel B" },
    }),
  );
  expect(contexts()[contexts().length - 1]).toMatchObject({
    activePeerDevices: [],
    activeChannel: "channel-b",
  });
  const calls = contexts().length;
  await act(async () => useAuth.setState({ generation: 2 }));
  expect(contexts().slice(-2)).toMatchObject([
    { activePeerDevices: [], activeChannel: null },
    { activePeerDevices: [], activeChannel: "channel-b" },
  ]);
  expect(contexts()).toHaveLength(calls + 2);
  await act(async () => root.unmount());
  expect(contexts()[contexts().length - 1]).toMatchObject({
    activePeerDevices: [],
    activeChannel: null,
  });
  const revisions = contexts().map((context) => context.revision as number);
  expect(
    revisions.every(
      (revision, index) => index === 0 || revision > revisions[index - 1],
    ),
  ).toBe(true);
});
