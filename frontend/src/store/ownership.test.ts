import { expect, it } from "vitest";
import { useAuth } from "./auth";
import { captureChatOwner } from "./ownership";

it("invalidates owned chat work when the runtime or identity changes", () => {
  useAuth.setState({ user: { id: "alice" } as never, generation: 7 });
  let snapshot = { runEpoch: 1, identityEpoch: 1 };
  const read = () => snapshot;
  const identitySensitive = captureChatOwner(read);
  const runtimeOnly = captureChatOwner(read, false);
  expect(identitySensitive.current()).toBe(true);
  snapshot = { runEpoch: 1, identityEpoch: 2 };
  expect(identitySensitive.current()).toBe(false);
  expect(runtimeOnly.current()).toBe(true);
  snapshot = { runEpoch: 2, identityEpoch: 2 };
  expect(runtimeOnly.current()).toBe(false);
});
