import { describe, it, expect, vi, beforeEach } from "vitest";

// The IPC contract: the frontend must call the exact Tauri command names with the exact
// (camelCase) arg keys the Rust commands expect. A typo here is a real runtime bug.
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { auth, avatars, chat, contactPolicy, favorites, settings } from "./api";
import { useAuth } from "@/store/auth";

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
  useAuth.setState({ user: null });
});

describe("media read IPC bytes", () => {
  it.each(["readMedia", "readFile"] as const)(
    "%s turns a fallback JSON byte array into binary image data",
    async (method) => {
      const pngHeader = [137, 80, 78, 71, 13, 10, 26, 10];
      invoke.mockResolvedValueOnce(pngHeader);

      const bytes = await chat[method]("file-conv");
      const blobBytes = new Uint8Array(await new Blob([bytes]).arrayBuffer());

      expect(blobBytes).toEqual(new Uint8Array(pngHeader));
    },
  );

  it("preserves the normal ArrayBuffer response", async () => {
    const original = new Uint8Array([137, 80, 78, 71]).buffer;
    invoke.mockResolvedValueOnce(original);

    expect(await chat.readMedia("file-conv")).toBe(original);
  });
});

describe("contact visibility command mapping", () => {
  it("binds reads and writes to the expected local owner", async () => {
    await contactPolicy.get("owner");
    expect(invoke).toHaveBeenCalledWith("get_hidden_contacts", {
      owner: "owner",
    });
    await contactPolicy.set("owner", "account", true, "Name");
    expect(invoke).toHaveBeenCalledWith("set_contact_hidden", {
      owner: "owner",
      account: "account",
      hidden: true,
      name: "Name",
    });
  });
});

it("uses the channel dissolution IPC command with its channel id", async () => {
  await chat.dissolveChannel("team");
  expect(invoke).toHaveBeenCalledWith("dissolve_channel", {
    channelId: "team",
  });
});

it("maps channel ownership transfer to the desktop command", async () => {
  await chat.transferChannelOwner("channel", "next-device");
  expect(invoke).toHaveBeenCalledWith("transfer_channel_owner", {
    channelId: "channel",
    memberId: "next-device",
  });
});

it("maps channel leave to the desktop command", async () => {
  await chat.leaveChannel("channel");
  expect(invoke).toHaveBeenCalledWith("leave_channel", {
    channelId: "channel",
  });
});

it("maps invitation acceptance to the desktop command", async () => {
  await chat.acceptChannelInvite("channel");
  expect(invoke).toHaveBeenCalledWith("accept_channel_invite", {
    channelId: "channel",
  });
});

it("loads the signed channel activity feed", async () => {
  await chat.channelActivity("channel");
  expect(invoke).toHaveBeenCalledWith("channel_activity", {
    channelId: "channel",
  });
});

describe("account contact metadata command mapping", () => {
  it("binds pin, alias, and avatar writes to the explicit owner", async () => {
    await favorites.setFavorite("alice", "peer", true);
    expect(invoke).toHaveBeenCalledWith("set_favorite", {
      owner: "alice",
      id: "peer",
      pinned: true,
    });
    await favorites.setAlias("alice", "peer", "Friend");
    expect(invoke).toHaveBeenCalledWith("set_alias", {
      owner: "alice",
      id: "peer",
      alias: "Friend",
    });
    await favorites.setChannelHidden("alice", "channel", true);
    expect(invoke).toHaveBeenCalledWith("set_channel_hidden", {
      owner: "alice",
      id: "channel",
      hidden: true,
    });
    await avatars.set("alice", "peer", "data:image/png;base64,AA==");
    expect(invoke).toHaveBeenCalledWith("set_avatar", {
      owner: "alice",
      id: "peer",
      dataUrl: "data:image/png;base64,AA==",
    });
  });

  it("binds verification reads and writes to the explicit owner", async () => {
    await chat.getTrust("alice", "peer", "fingerprint");
    expect(invoke).toHaveBeenCalledWith("get_trust", {
      owner: "alice",
      accountId: "peer",
      currentFingerprint: "fingerprint",
    });
    await chat.markVerified("alice", "peer", "fingerprint");
    expect(invoke).toHaveBeenCalledWith("mark_verified", {
      owner: "alice",
      accountId: "peer",
      fingerprint: "fingerprint",
    });
  });
});

describe("settings updates", () => {
  it("captures the owner before a queued update crosses a login", async () => {
    useAuth.setState({
      user: { id: "alice", username: "alice", display_name: "Alice" },
    });
    let release!: () => void;
    invoke.mockImplementation((command: string) =>
      command === "get_app_settings"
        ? new Promise((resolve) => {
            release = () => resolve({ notifications: true });
          })
        : Promise.resolve(),
    );
    const pending = settings.update({ notifications: false });
    await vi.waitFor(() => expect(release).toBeDefined());
    useAuth.setState({
      user: { id: "bob", username: "bob", display_name: "Bob" },
    });
    release();
    await pending;
    expect(invoke).toHaveBeenCalledWith("set_app_settings", {
      owner: "alice",
      settings: { notifications: false },
    });
  });
  it("preserves both fields when two updates are requested before the first write finishes", async () => {
    const persisted = { calls_enabled: false, notifications: true };
    let releaseFirst!: () => void;
    let writes = 0;
    invoke.mockImplementation(
      (command: string, args?: { settings: typeof persisted }) => {
        if (command === "get_app_settings")
          return Promise.resolve({ ...persisted });
        if (command === "set_app_settings") {
          writes++;
          if (writes === 1)
            return new Promise<void>((resolve) => {
              releaseFirst = () => {
                Object.assign(persisted, args!.settings);
                resolve();
              };
            });
          Object.assign(persisted, args!.settings);
          return Promise.resolve();
        }
        return Promise.resolve();
      },
    );

    const first = settings.update({ calls_enabled: true });
    const second = settings.update({ notifications: false });
    await vi.waitFor(() => expect(releaseFirst).toBeDefined());
    releaseFirst();
    await Promise.all([first, second]);

    expect(persisted).toMatchObject({
      calls_enabled: true,
      notifications: false,
    });
  });

  it("continues applying later updates after an earlier write fails", async () => {
    const persisted = { notifications: true };
    let writes = 0;
    invoke.mockImplementation(
      (command: string, args?: { settings: typeof persisted }) => {
        if (command === "get_app_settings")
          return Promise.resolve({ ...persisted });
        if (command === "set_app_settings") {
          if (++writes === 1) return Promise.reject(new Error("disk full"));
          Object.assign(persisted, args!.settings);
        }
        return Promise.resolve();
      },
    );

    const failed = settings.update({ notifications: false });
    const later = settings.update({ notifications: false });
    await expect(failed).rejects.toThrow("disk full");
    await later;
    expect(persisted.notifications).toBe(false);
  });
});

describe("auth command mapping", () => {
  it("login", async () => {
    await auth.login("u", "p");
    expect(invoke).toHaveBeenCalledWith("login", {
      username: "u",
      password: "p",
    });
  });
  it("register", async () => {
    await auth.register("u", "p");
    expect(invoke).toHaveBeenCalledWith("register", {
      username: "u",
      password: "p",
    });
  });
  it("logout / adopt take no args", async () => {
    await auth.logout();
    expect(invoke).toHaveBeenCalledWith("logout");
    await auth.adoptLinkedAccount();
    expect(invoke).toHaveBeenCalledWith("adopt_linked_account");
  });
  it("saved-login retry carries an explicit owner and no password", async () => {
    await auth.retrySavedSession("owner-1");
    expect(invoke).toHaveBeenCalledWith("retry_saved_session", {
      owner: "owner-1",
    });
  });
  it("auto_login / clear_saved_session take no args", async () => {
    await auth.autoLogin();
    expect(invoke).toHaveBeenCalledWith("auto_login");
    await auth.clearSavedSession();
    expect(invoke).toHaveBeenCalledWith("clear_saved_session");
  });
});

describe("chat command mapping", () => {
  it("my_id / account_id", async () => {
    await chat.myId();
    expect(invoke).toHaveBeenCalledWith("my_id");
    await chat.accountId();
    expect(invoke).toHaveBeenCalledWith("account_id");
  });
  it("send_dm maps replyTo (and defaults it to null)", async () => {
    await chat.sendDm("r", "hello", "ev1");
    expect(invoke).toHaveBeenCalledWith("send_dm", {
      recipient: "r",
      text: "hello",
      replyTo: "ev1",
    });
    await chat.sendDm("r", "hello");
    expect(invoke).toHaveBeenLastCalledWith("send_dm", {
      recipient: "r",
      text: "hello",
      replyTo: null,
    });
  });
  it("send_to_account", async () => {
    await chat.sendToAccount("acct", "hi", null);
    expect(invoke).toHaveBeenCalledWith("send_to_account", {
      account: "acct",
      text: "hi",
      replyTo: null,
    });
  });
  it("create_channel maps memberIds", async () => {
    await chat.createChannel("general", ["a", "b"]);
    expect(invoke).toHaveBeenCalledWith("create_channel", {
      name: "general",
      memberIds: ["a", "b"],
    });
  });
  it("react_channel maps channelId/target/emoji/remove", async () => {
    await chat.reactChannel("chan", "tgt", "👍", true);
    expect(invoke).toHaveBeenCalledWith("react_channel", {
      channelId: "chan",
      target: "tgt",
      emoji: "👍",
      remove: true,
    });
  });
  it("save_file maps fileConv/dest", async () => {
    await chat.saveFile("fc", "/tmp/out");
    expect(invoke).toHaveBeenCalledWith("save_file", {
      fileConv: "fc",
      dest: "/tmp/out",
    });
  });
  it("opens only the current owner's configured download folder", async () => {
    await chat.openDownloadDir("alice");
    expect(invoke).toHaveBeenCalledWith("open_download_dir", {
      owner: "alice",
    });
  });
  it("maps per-file sender keys and batch receive status to the Tauri IPC contract", async () => {
    await chat.enqueueFile("owner", "account", "/tmp/a.bin", false, "c1");
    expect(invoke).toHaveBeenLastCalledWith("owner_enqueue_file", {
      owner: "owner",
      account: "account",
      path: "/tmp/a.bin",
      media: false,
      progressKey: "c1",
    });
    await chat.sendFileChannel("channel", "/tmp/b.bin", true, "c2");
    expect(invoke).toHaveBeenLastCalledWith("send_file_channel", {
      channelId: "channel",
      path: "/tmp/b.bin",
      media: true,
      progressKey: "c2",
    });
    await chat.fileStatuses(["file-a", "file-b"]);
    expect(invoke).toHaveBeenLastCalledWith("file_statuses", {
      fileConvs: ["file-a", "file-b"],
    });
  });
  it("link_device maps peer/code", async () => {
    await chat.linkDevice("peer1", "CODE");
    expect(invoke).toHaveBeenCalledWith("link_device", {
      peer: "peer1",
      code: "CODE",
    });
  });
});

it("deletes an offline contact under the expected owner", async () => {
  await contactPolicy.delete("owner", "account");
  expect(invoke).toHaveBeenCalledWith("delete_contact", {
    owner: "owner",
    account: "account",
  });
});

it("carries the expected owner on all linked synchronization commands", async () => {
  await chat.getDeviceSyncPolicy("owner-a");
  expect(invoke).toHaveBeenLastCalledWith("get_device_sync_policy", {
    owner: "owner-a",
  });
  await chat.getDeviceSyncStatus("owner-a");
  expect(invoke).toHaveBeenLastCalledWith("get_device_sync_status", {
    owner: "owner-a",
  });
  await chat.setDeviceSyncCategory("history", false, "owner-a");
  expect(invoke).toHaveBeenLastCalledWith("set_device_sync_category", {
    category: "history",
    enabled: false,
    owner: "owner-a",
  });
  await chat.getAccountPreferences("appearance", "owner-a");
  expect(invoke).toHaveBeenLastCalledWith("get_account_preferences", {
    category: "appearance",
    owner: "owner-a",
  });
  await chat.setAccountPreference("appearance", "theme", "dark", "owner-a");
  expect(invoke).toHaveBeenLastCalledWith("set_account_preference", {
    category: "appearance",
    key: "theme",
    value: "dark",
    owner: "owner-a",
  });
});

describe("optional delivery scheduling observations", () => {
  it.each([
    { id: "message", status: "awaiting" },
    {
      id: "message",
      status: "awaiting",
      observation: { kind: "attempting", age_ms: 12 },
    },
    {
      id: "message",
      status: "awaiting",
      observation: { kind: "retry_scheduled", age_ms: 120 },
    },
  ])("preserves additive and older host responses: %j", async (row) => {
    invoke.mockResolvedValueOnce([row]);
    expect(await chat.deliveryStatuses("alice", "peer", ["message"])).toEqual([
      row,
    ]);
    expect(invoke).toHaveBeenCalledWith("owner_delivery_statuses", {
      owner: "alice",
      account: "peer",
      ids: ["message"],
    });
  });
});

it("preserves exact search namespaces and owner payloads", async () => {
  await chat.searchRecent("owner", "needle");
  expect(invoke).toHaveBeenLastCalledWith("owner_search_recent", {
    owner: "owner",
    query: "needle",
  });
  for (const kind of ["account", "channel", "legacy-device"] as const) {
    const reference = { kind, target: "same-bytes", message_id: "canonical" };
    await chat.searchContext("owner", reference);
    expect(invoke).toHaveBeenLastCalledWith("owner_search_context", {
      owner: "owner",
      reference,
    });
  }
});

it("sends each channel history policy through the typed IPC command", async () => {
  for (const policy of [
    "off",
    "last_ten",
    "last_three_days",
    "last_ten_within_three_days",
  ] as const) {
    await chat.setChannelHistoryPolicy("channel", policy);
    expect(invoke).toHaveBeenLastCalledWith("set_channel_history_policy", {
      channelId: "channel",
      policy,
    });
  }
});
