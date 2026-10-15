import { beforeEach, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({
  invoke: vi.fn((_command: string): Promise<unknown> => Promise.resolve()),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { useAppLock } from "./appLock";
import { useAuth } from "./auth";
import { useChat } from "./chat";
import { useTransfers } from "./transfers";
import { useFileAvailability } from "./fileAvailability";

beforeEach(() => {
  useAppLock.setState({ locked: false, busy: false, error: null });
  useAuth.setState({
    user: { id: "owner", username: "owner", display_name: "Owner" },
    generation: 0,
  });
  useChat.setState({
    runEpoch: 0,
    ready: true,
    myId: "private-device",
    myAccountId: "private-account",
    active: { kind: "channel", id: "group", name: "Private group" },
    channelMembersById: {
      group: [{ user_id: "friend", name: "Friend" }],
    },
    unread: { "channel:group": 3 },
    unreadOwner: "owner",
    pendingUnknownUnread: { unknown: 1 },
    historyRequests: { "channel:group": 1 },
    cacheOrder: ["channel:group"],
    channelOwner: "private-device",
  });
  useTransfers.getState().reset();
  useTransfers.getState().applyProgress({
    file_conv: "private-file",
    direction: "send",
    done: 1,
    total: 10,
  });
  useFileAvailability.getState().reset();
  useFileAvailability
    .getState()
    .merge([{ file_conv: "private-file", ready: true, done: 10, total: 10 }]);
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
});

it("clears session chat and file metadata when locking", async () => {
  await useAppLock.getState().lock();
  const chat = useChat.getState();
  expect(useAppLock.getState().locked).toBe(true);
  expect(chat.ready).toBe(false);
  expect(chat.myId).toBe("");
  expect(chat.myAccountId).toBe("");
  expect(chat.channelMembersById).toEqual({});
  expect(chat.unread).toEqual({});
  expect(chat.unreadOwner).toBe("");
  expect(chat.pendingUnknownUnread).toEqual({});
  expect(chat.historyRequests).toEqual({});
  expect(chat.cacheOrder).toEqual([]);
  expect(chat.channelOwner).toBe("");
  expect(useTransfers.getState().transfers).toEqual({});
  expect(useFileAvailability.getState().statuses).toEqual({});
  expect(chat.runEpoch).toBe(1);
  expect(invoke).toHaveBeenCalledWith("lock_app");
});

it("keeps a late roster response from restoring locked session data", async () => {
  let finishPeers!: (peers: unknown[]) => void;
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "owner",
        device_id: "private-device",
        account_id: "private-account",
      });
    if (command === "list_peers")
      return new Promise((resolve) => {
        finishPeers = resolve;
      });
    if (command === "lock_status")
      return Promise.resolve(useAuth.getState().user);
    return Promise.resolve([]);
  });

  const roster = useChat.getState().refreshRoster();
  await vi.waitFor(() => expect(finishPeers).toBeDefined());
  await useAppLock.getState().refresh();
  finishPeers([
    {
      user_id: "late-peer",
      account_id: "late-account",
      name: "Late peer",
      addr: "127.0.0.1:47475",
      post_office: false,
    },
  ]);
  await roster;

  expect(useAppLock.getState().locked).toBe(true);
  expect(useChat.getState().peers).toEqual([]);
  expect(useChat.getState().ready).toBe(false);
});
