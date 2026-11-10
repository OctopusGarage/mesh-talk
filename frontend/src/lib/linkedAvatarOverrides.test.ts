import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { accountDocuments } from "./api/accountDocuments";
import { avatars } from "./api/contacts";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { useAvatars } from "@/store/avatars";
import {
  startLinkedAvatarOverrides,
  publishAvatarOverridesBeforeLink,
} from "./linkedAvatarOverrides";
vi.mock("./api/accountDocuments", () => ({
  accountDocuments: { list: vi.fn() },
}));
vi.mock("./api/contacts", () => ({ avatars: { publishLegacy: vi.fn() } }));
let stop: (() => void) | undefined;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("window", globalThis);
  useAuth.setState({ user: { id: "owner" } as never, generation: 1 });
  useChat.setState({
    myAccountId: "aa".repeat(16),
    runEpoch: 1,
    identityEpoch: 1,
  });
  vi.spyOn(useAvatars.getState(), "load").mockResolvedValue(true);
  vi.mocked(accountDocuments.list).mockResolvedValue({
    items: [],
    next: null,
    digest: [1],
  });
  vi.mocked(avatars.publishLegacy).mockResolvedValue(1);
});
afterEach(() => {
  stop?.();
  stop = undefined;
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});
it("uses the existing domain digest to avoid repeatedly reading unchanged images", async () => {
  const load = useAvatars.getState().load;
  stop = startLinkedAvatarOverrides("owner");
  await vi.advanceTimersByTimeAsync(0);
  expect(load).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(10_000);
  expect(load).toHaveBeenCalledTimes(1);
  vi.mocked(accountDocuments.list).mockResolvedValue({
    items: [],
    next: null,
    digest: [2],
  });
  await vi.advanceTimersByTimeAsync(10_000);
  expect(load).toHaveBeenCalledTimes(2);
});
it("does not apply a delayed old-account digest after adoption", async () => {
  let resolve!: (page: {
    items: never[];
    next: null;
    digest: number[];
  }) => void;
  vi.mocked(accountDocuments.list).mockReturnValue(
    new Promise((done) => {
      resolve = done;
    }),
  );
  const load = useAvatars.getState().load;
  stop = startLinkedAvatarOverrides("owner");
  useChat.setState({ myAccountId: "bb".repeat(16), identityEpoch: 2 });
  resolve({ items: [], next: null, digest: [1] });
  await vi.advanceTimersByTimeAsync(0);
  expect(load).not.toHaveBeenCalled();
});
it("attributes legacy images only during an explicit source preparation", async () => {
  stop = startLinkedAvatarOverrides("owner");
  await vi.advanceTimersByTimeAsync(0);
  expect(avatars.publishLegacy).not.toHaveBeenCalled();
  await publishAvatarOverridesBeforeLink("owner");
  expect(avatars.publishLegacy).toHaveBeenCalledWith("owner", "aa".repeat(16));
});

it("retries the same digest after a failed projection read", async () => {
  const load = vi.mocked(useAvatars.getState().load);
  load.mockResolvedValueOnce(false).mockResolvedValue(true);
  stop = startLinkedAvatarOverrides("owner");
  await vi.advanceTimersByTimeAsync(0);
  await vi.advanceTimersByTimeAsync(10_000);
  expect(load).toHaveBeenCalledTimes(2);
  await vi.advanceTimersByTimeAsync(10_000);
  expect(load).toHaveBeenCalledTimes(2);
});
it("retries digest observation after a stalled read and ignores its late response", async () => {
  let finish!: (page: { items: never[]; next: null; digest: number[] }) => void;
  vi.mocked(accountDocuments.list).mockReturnValueOnce(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  const load = useAvatars.getState().load;
  stop = startLinkedAvatarOverrides("owner");
  try {
    await vi.advanceTimersByTimeAsync(10_001);
    expect(accountDocuments.list).toHaveBeenCalledTimes(2);
    expect(load).toHaveBeenCalledOnce();
    finish({ items: [], next: null, digest: [2] });
    await vi.advanceTimersByTimeAsync(0);
    expect(load).toHaveBeenCalledOnce();
  } finally {
    finish({ items: [], next: null, digest: [2] });
    await vi.advanceTimersByTimeAsync(0);
  }
});
