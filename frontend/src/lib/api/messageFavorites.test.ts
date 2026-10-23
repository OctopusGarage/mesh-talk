import { beforeEach, expect, it, vi } from "vitest";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { messageFavorites } from "./messageFavorites";
beforeEach(() => invoke.mockReset());
it("sends canonical references and personal notes with explicit owner and edit revision", async () => {
  const source = {
    kind: "channel" as const,
    target: "channel",
    message_ids: ["a", "b"],
  };
  const metadata = {
    title: "Runbook",
    description: "Why this matters",
    notes: { a: "Critical" },
  };
  await messageFavorites.save("alice", source, metadata);
  expect(invoke).toHaveBeenLastCalledWith("owner_save_message_favorite", {
    owner: "alice",
    source,
    metadata,
  });
  await messageFavorites.edit("alice", "f", 42, "device", metadata);
  expect(invoke).toHaveBeenLastCalledWith("owner_edit_message_favorite", {
    owner: "alice",
    id: "f",
    expectedRevision: 42,
    expectedDeviceId: "device",
    metadata,
  });
  await messageFavorites.inspect("alice", source);
  expect(invoke).toHaveBeenLastCalledWith("owner_inspect_message_favorites", {
    owner: "alice",
    source,
  });
});
it.each([new Uint8Array([0, 255, 7]).buffer, [0, 255, 7]])(
  "normalizes retained bytes from both Tauri IPC transports",
  async (bytes) => {
    invoke.mockResolvedValueOnce(bytes);
    const result = await messageFavorites.readFile("alice", "fc");
    expect([...new Uint8Array(result)]).toEqual([0, 255, 7]);
    expect(invoke).toHaveBeenLastCalledWith("owner_read_favorite_file", {
      owner: "alice",
      fileConv: "fc",
    });
  },
);
