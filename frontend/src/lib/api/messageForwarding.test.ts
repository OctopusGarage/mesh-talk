import { beforeEach, expect, it, vi } from "vitest";
const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { messageForwarding, type ForwardRequest } from "./messageForwarding";
beforeEach(() => invoke.mockReset());
it("passes owner and stable source identities without content", async () => {
  const source = {
    kind: "account" as const,
    target: "alice",
    message_ids: ["one"],
  };
  await messageForwarding.inspect("owner", source);
  expect(invoke).toHaveBeenLastCalledWith("owner_inspect_forward_messages", {
    owner: "owner",
    source,
  });
  const request: ForwardRequest = {
    operation_id: "stable",
    source,
    destinations: [{ kind: "account", target: "bob" }],
    mode: "individual",
  };
  await messageForwarding.send("owner", request);
  await messageForwarding.send("owner", request);
  expect(invoke).toHaveBeenLastCalledWith("owner_forward_messages", {
    owner: "owner",
    request,
  });
  await messageForwarding.list("owner");
  expect(invoke).toHaveBeenLastCalledWith("owner_list_forward_batches", {
    owner: "owner",
  });
});
