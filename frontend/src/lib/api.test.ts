import { describe, it, expect, vi, beforeEach } from "vitest";

// The IPC contract: the frontend must call the exact Tauri command names with the exact
// (camelCase) arg keys the Rust commands expect. A typo here is a real runtime bug.
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { auth, avatars, chat, contactPolicy, favorites, settings } from "./api";
import { createSettingsApi } from "./api/settings";
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

it("removes channel account membership with explicit channel and account identities", async () => {
  await chat.removeChannelAccountMembers("team", "peer-account");
  expect(invoke).toHaveBeenCalledWith("remove_channel_account_members", {
    channelId: "team",
    accountId: "peer-account",
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
    await expect(pending).rejects.toThrow("Settings session changed");
    expect(
      invoke.mock.calls.filter(([command]) => command === "set_app_settings"),
    ).toHaveLength(0);
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
  it("create_channel maps memberAccountIds", async () => {
    await chat.createChannel("general", ["a", "b"]);
    expect(invoke).toHaveBeenCalledWith("create_channel", {
      name: "general",
      memberAccountIds: ["a", "b"],
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
      syncAll: false,
    });
  });
});

it("binds contact reopening to the supplied full account", async () => {
  await contactPolicy.set("owner", "account", false, "Name", "own-account");
  expect(invoke).toHaveBeenCalledWith("set_contact_hidden", {
    owner: "owner",
    account: "account",
    hidden: false,
    name: "Name",
    expectedAccount: "own-account",
  });
});

it("deletes a contact under the expected owner and full account", async () => {
  await contactPolicy.delete("owner", "own-account", "account");
  expect(invoke).toHaveBeenCalledWith("delete_contact", {
    owner: "owner",
    expectedAccount: "own-account",
    account: "account",
    clearHistory: true,
  });
});
it("keeps contact history when explicitly requested", async () => {
  await contactPolicy.delete("owner", "own-account", "account", false);
  expect(invoke).toHaveBeenCalledWith("delete_contact", {
    owner: "owner",
    expectedAccount: "own-account",
    account: "account",
    clearHistory: false,
  });
});
it("deletes an exited group chat under its exact account and owner", async () => {
  await chat.deleteLeftChannel("owner", "account", "group");
  expect(invoke).toHaveBeenCalledWith("owner_delete_left_channel", {
    owner: "owner",
    expectedAccount: "account",
    channelId: "group",
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
  await chat.setAccountPreference(
    "appearance",
    "theme",
    "dark",
    "owner-a",
    "source-account",
  );
  expect(invoke).toHaveBeenLastCalledWith("set_account_preference", {
    category: "appearance",
    key: "theme",
    value: "dark",
    owner: "owner-a",
    expectedAccount: "source-account",
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

it("binds protected sends and reveals to the exact owner and stable message reference", async () => {
  await chat.enqueueText("owner", "account", "secret", "reply", null, "click");
  expect(invoke).toHaveBeenLastCalledWith("owner_enqueue_text", {
    owner: "owner",
    account: "account",
    text: "secret",
    replyTo: "reply",
    expiresAtMs: null,
    protection: "click",
  });
  await chat.enqueueChannelText(
    "owner",
    "channel",
    "secret",
    null,
    null,
    "authenticate",
  );
  expect(invoke).toHaveBeenLastCalledWith(
    "owner_enqueue_channel_text",
    expect.objectContaining({ protection: "authenticate" }),
  );
  const reference = {
    kind: "account" as const,
    target: "account",
    message_id: "message",
  };
  await chat.revealMessage("owner", reference, "password");
  expect(invoke).toHaveBeenLastCalledWith("owner_reveal_message", {
    owner: "owner",
    reference,
    password: "password",
  });
});

it("keeps protected image bytes behind the owner-scoped binary reveal command", async () => {
  const reference = {
    kind: "channel" as const,
    target: "channel",
    message_id: "image",
  };
  invoke.mockResolvedValueOnce([137, 80, 78, 71]);
  expect(
    new Uint8Array(await chat.revealImage("owner", reference, "password")),
  ).toEqual(new Uint8Array([137, 80, 78, 71]));
  expect(invoke).toHaveBeenLastCalledWith("owner_reveal_image", {
    owner: "owner",
    reference,
    password: "password",
  });
  await chat.enqueueFile(
    "owner",
    "peer",
    "/tmp/photo.png",
    true,
    "progress",
    null,
    "click",
  );
  expect(invoke).toHaveBeenLastCalledWith(
    "owner_enqueue_file",
    expect.objectContaining({ protection: "click" }),
  );
});

it("binds linked contact imports to the captured cryptographic account as well as the local owner", async () => {
  const rows = [
    { id: "contact", pinned: false, custom_alias: "Ronaldo", hidden: false },
  ];
  await favorites.apply("local-owner", "captured-account", rows, {
    contact: null,
  });
  expect(invoke).toHaveBeenLastCalledWith("set_favorite_records", {
    owner: "local-owner",
    expectedAccount: "captured-account",
    records: rows,
    expected: { contact: null },
  });
  const hidden = [{ account: "contact", hidden: true, name: "Peer" }];
  await contactPolicy.apply("local-owner", "captured-account", hidden, {
    contact: null,
  });
  expect(invoke).toHaveBeenLastCalledWith("set_contact_hidden_batch", {
    owner: "local-owner",
    expectedAccount: "captured-account",
    records: hidden,
    expected: { contact: null },
  });
});

it("keeps unauthenticated device enrollment bound to its native attempt and verified source", async () => {
  await auth.beginDeviceEnrollment();
  expect(invoke).toHaveBeenLastCalledWith("begin_device_enrollment");
  await auth.probeDeviceEnrollment("attempt", ["192.0.2.4:47475"]);
  expect(invoke).toHaveBeenLastCalledWith("probe_device_enrollment", {
    enrollmentId: "attempt",
    targets: ["192.0.2.4:47475"],
  });
  await auth.completeDeviceEnrollment(
    "attempt",
    "source",
    "a".repeat(32),
    false,
  );
  expect(invoke).toHaveBeenLastCalledWith("complete_device_enrollment", {
    enrollmentId: "attempt",
    sourceId: "source",
    code: "a".repeat(32),
    includeHistory: false,
  });
  await auth.cancelDeviceEnrollment("attempt");
  expect(invoke).toHaveBeenLastCalledWith("cancel_device_enrollment", {
    enrollmentId: "attempt",
  });
});

it("reads nonsecret shared-login progress under the explicit local owner", async () => {
  await chat.getAccountLoginSyncStatus("owner");
  expect(invoke).toHaveBeenLastCalledWith("get_account_login_sync_status", {
    owner: "owner",
  });
});

it("rejects a stale cryptographic account while a settings update is queued", async () => {
  let account = "original-account";
  const api = createSettingsApi(
    () => "local-owner",
    () => account,
  );
  let finish!: (value: unknown) => void;
  invoke.mockImplementation((command: string) =>
    command === "get_app_settings"
      ? new Promise((resolve) => {
          finish = resolve;
        })
      : Promise.resolve(),
  );
  const pending = api.update({ notifications: false });
  await vi.waitFor(() => expect(finish).toBeDefined());
  account = "replacement-account";
  finish({ notifications: true });
  await expect(pending).rejects.toThrow("Settings session changed");
  expect(
    invoke.mock.calls.filter(([command]) => command === "set_app_settings"),
  ).toHaveLength(0);
});

it("rejects a same-owner same-account relogin before a queued settings write", async () => {
  let generation = 1;
  const api = createSettingsApi(
    () => "owner",
    () => "account",
    () => generation,
  );
  let finish!: (value: unknown) => void;
  invoke.mockImplementation((command: string) =>
    command === "get_app_settings"
      ? new Promise((resolve) => {
          finish = resolve;
        })
      : Promise.resolve(),
  );
  const pending = api.update({ notifications: false });
  await vi.waitFor(() => expect(finish).toBeDefined());
  generation++;
  finish({ notifications: true });
  await expect(pending).rejects.toThrow("Settings session changed");
  expect(
    invoke.mock.calls.filter(([command]) => command === "set_app_settings"),
  ).toHaveLength(0);
});

it("binds settings reads and edits to the current cryptographic account", async () => {
  const api = createSettingsApi(
    () => "owner",
    () => "account",
    () => 1,
  );
  invoke.mockResolvedValue({ notifications: true });
  await api.update({ notifications: false });
  expect(invoke).toHaveBeenCalledWith("get_app_settings", {
    owner: "owner",
    expectedAccount: "account",
  });
  expect(invoke).toHaveBeenCalledWith("set_app_settings", {
    owner: "owner",
    expectedAccount: "account",
    settings: { notifications: false },
    patch: { notifications: false },
  });
});

it("keeps account group operation acceptance separate from public event commitment", async () => {
  const pending = {
    id: "operation-id",
    account_id: "account",
    group: "channel",
    kind: "rename",
    phase: { kind: "awaiting_acceptance" },
    executions: [],
    committed_events: [],
    primary_event: null,
  };
  vi.mocked(invoke).mockResolvedValueOnce(pending);
  expect(
    await chat.submitAccountGroupControl(
      "owner",
      "account",
      "operation-id",
      "channel",
      { kind: "rename", name: "New name" },
    ),
  ).toEqual(pending);
  expect(invoke).toHaveBeenLastCalledWith(
    "owner_submit_account_group_control",
    {
      owner: "owner",
      expectedAccount: "account",
      operationId: "operation-id",
      channelId: "channel",
      operation: { kind: "rename", name: "New name" },
    },
  );
  await chat.accountGroupOperationStatus("owner", "account", "operation-id");
  expect(invoke).toHaveBeenLastCalledWith(
    "owner_account_group_operation_status",
    {
      owner: "owner",
      expectedAccount: "account",
      operationId: "operation-id",
    },
  );
  await chat.listAccountGroupOperations("owner", "account", {
    channelId: "channel",
    pendingOnly: true,
    after: "previous-operation-id",
    limit: 25,
  });
  expect(invoke).toHaveBeenLastCalledWith(
    "owner_list_account_group_operations",
    {
      owner: "owner",
      expectedAccount: "account",
      channelId: "channel",
      pendingOnly: true,
      after: "previous-operation-id",
      limit: 25,
    },
  );
  await chat.cancelAccountGroupOperation("owner", "account", "operation-id");
  expect(invoke).toHaveBeenLastCalledWith(
    "owner_cancel_account_group_operation",
    {
      owner: "owner",
      expectedAccount: "account",
      operationId: "operation-id",
    },
  );
});

it("mints a scoped operation ID before submitting a group intent", async () => {
  const id = "ab".repeat(32);
  vi.mocked(invoke).mockResolvedValueOnce(id);
  expect(await chat.newAccountGroupOperationId("owner", "account")).toBe(id);
  expect(invoke).toHaveBeenLastCalledWith(
    "owner_new_account_group_operation_id",
    {
      owner: "owner",
      expectedAccount: "account",
    },
  );
});

it("binds group creation to the captured owner and account", async () => {
  vi.mocked(invoke).mockResolvedValueOnce("new-group");
  expect(
    await chat.createChannelForOwner("owner", "account", "Group", ["peer"]),
  ).toBe("new-group");
  expect(invoke).toHaveBeenLastCalledWith("owner_create_channel", {
    owner: "owner",
    expectedAccount: "account",
    name: "Group",
    memberAccountIds: ["peer"],
  });
});

it("submits text and sticker group operations with owner/account identity and original operation IDs", async () => {
  const status = {
    id: "operation",
    phase: { kind: "accepted" },
    primary_event: null,
  };
  invoke.mockResolvedValue(status);
  expect(
    await chat.submitAccountGroupText(
      "owner",
      "account",
      "operation",
      "group",
      "hello",
      "reply",
      123,
    ),
  ).toBe(status);
  expect(invoke).toHaveBeenLastCalledWith("owner_submit_account_group_text", {
    owner: "owner",
    expectedAccount: "account",
    operationId: "operation",
    channelId: "group",
    text: "hello",
    replyTo: "reply",
    expiresAtMs: 123,
  });
  expect(
    await chat.submitAccountGroupSticker(
      "owner",
      "account",
      "operation",
      "group",
      "heart",
      "❤️",
      123,
    ),
  ).toBe(status);
  expect(invoke).toHaveBeenLastCalledWith(
    "owner_submit_account_group_sticker",
    {
      owner: "owner",
      expectedAccount: "account",
      operationId: "operation",
      channelId: "group",
      stickerId: "heart",
      fallback: "❤️",
      expiresAtMs: 123,
    },
  );
});

it("submits raw File account operation arguments without discarding pending lifecycle", async () => {
  const pending = {
    id: "original",
    phase: { kind: "waiting_for_executor" },
    primary_event: null,
  };
  invoke.mockResolvedValue(pending);
  expect(
    await chat.submitAccountGroupFile(
      "owner",
      "account",
      "original",
      "group",
      "/tmp/source.png",
      true,
      "client",
      123,
      "authenticate",
    ),
  ).toBe(pending);
  expect(invoke).toHaveBeenLastCalledWith("owner_submit_account_group_file", {
    owner: "owner",
    expectedAccount: "account",
    operationId: "original",
    channelId: "group",
    path: "/tmp/source.png",
    media: true,
    progressKey: "client",
    expiresAtMs: 123,
    protection: "authenticate",
  });
});

it("reads personal conversation controls under the exact owner and account", async () => {
  await chat.personalConversationStates("owner", "account");
  expect(invoke).toHaveBeenCalledWith("owner_personal_conversation_states", {
    owner: "owner",
    expectedAccount: "account",
  });
});
