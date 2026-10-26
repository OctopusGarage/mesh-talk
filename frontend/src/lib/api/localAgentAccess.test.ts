import { expect, it, vi } from "vitest";
const invoke = vi.hoisted(() => vi.fn().mockResolvedValue({}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { localAgentAccess } from "./localAgentAccess";
it("carries the explicit settings owner and exact selected conversation identities", async () => {
  await localAgentAccess.get("alice");
  expect(invoke).toHaveBeenLastCalledWith("get_local_agent_access", {
    owner: "alice",
  });
  const destinations = [{ kind: "channel" as const, id: "ab".repeat(32) }];
  await localAgentAccess.set("alice", true, destinations);
  expect(invoke).toHaveBeenLastCalledWith("set_local_agent_access", {
    owner: "alice",
    enabled: true,
    destinations,
  });
});

it("only passes renewal and duration when explicitly requested", async () => {
  await localAgentAccess.set("alice", true, [], {
    lifetime: "one_hour",
    renew: true,
  });
  expect(invoke).toHaveBeenLastCalledWith("set_local_agent_access", {
    owner: "alice",
    enabled: true,
    destinations: [],
    lifetime: "one_hour",
    renew: true,
  });
});
