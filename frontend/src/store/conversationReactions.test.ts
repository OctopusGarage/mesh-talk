import { beforeEach, expect, it, vi } from "vitest";
const { summaries } = vi.hoisted(() => ({ summaries: vi.fn() }));
vi.mock("@/lib/api/chat", () => ({ chat: { reactionSummaries: summaries } }));
import { useAuth } from "./auth";
import {
  readReactionPage,
  requestConversationReactions,
} from "./conversationReactions";
import type { ReactionInfo, ReactionSummaryPage } from "@/lib/types";
const id = (number: number) => number.toString(16).padStart(64, "0");
const conversation = { kind: "account", id: "friend", name: "Friend" } as const;
const page = (target: string, emoji = "👍"): ReactionSummaryPage => ({
  items: [{ target, emoji, count: 1, from_me: false }],
  next: null,
  pending: false,
});
function fixture() {
  const state = {
    active: conversation,
    runEpoch: 1,
    identityEpoch: 1,
    activeRequest: 1,
    reactions: {} as Record<string, ReactionInfo[]>,
    reactionRemainder: {} as Record<string, string[]>,
  };
  return {
    state,
    get: () => state,
    set: (patch: Partial<typeof state>) => Object.assign(state, patch),
  };
}
beforeEach(() => {
  summaries.mockReset();
  useAuth.setState({
    user: { id: "host", username: "a", display_name: "A" },
    generation: 0,
  });
});
it("coalesces invalidation bursts while keeping one physical native summary read", async () => {
  const f = fixture();
  const finishes: Array<(value: ReactionSummaryPage) => void> = [];
  summaries.mockImplementation(
    () => new Promise((resolve) => finishes.push(resolve)),
  );
  for (let number = 1; number <= 100; number++)
    requestConversationReactions(f.set, f.get, conversation, [id(number)]);
  expect(summaries).toHaveBeenCalledTimes(1);
  finishes[0](page(id(1)));
  await vi.waitFor(() => expect(finishes).toHaveLength(2));
  expect(summaries.mock.calls[1][3]).toEqual([id(100)]);
  finishes[1](page(id(100)));
  await vi.waitFor(() =>
    expect(f.state.reactions["account:friend"]).toEqual(page(id(100)).items),
  );
  expect(summaries).toHaveBeenCalledTimes(2);
});
it("drops an old account generation and resumes only its latest scoped demand", async () => {
  const f = fixture();
  const finishes: Array<(value: ReactionSummaryPage) => void> = [];
  summaries.mockImplementation(
    () => new Promise((resolve) => finishes.push(resolve)),
  );
  requestConversationReactions(f.set, f.get, conversation, [id(1)]);
  f.state.identityEpoch++;
  requestConversationReactions(f.set, f.get, conversation, [id(2)]);
  finishes[0](page(id(1)));
  await vi.waitFor(() => expect(finishes).toHaveLength(2));
  expect(f.state.reactions["account:friend"]).toEqual([]);
  finishes[1](page(id(2)));
  await vi.waitFor(() =>
    expect(f.state.reactions["account:friend"]?.[0]?.target).toBe(id(2)),
  );
});
it("retains only a bounded summary prefix and exposes unfinished targets", async () => {
  const f = fixture();
  const targets = Array.from({ length: 500 }, (_, number) => id(number));
  summaries.mockImplementation((_owner, _kind, _target, selected: string[]) =>
    Promise.resolve({
      items: Array.from({ length: 128 }, (_, index) => ({
        target: selected[index % selected.length],
        emoji: String(index),
        count: 100000,
        from_me: false,
      })),
      next: null,
      pending: false,
    }),
  );
  requestConversationReactions(f.set, f.get, conversation, targets);
  await vi.waitFor(() =>
    expect(f.state.reactions["account:friend"]).toHaveLength(512),
  );
  expect(summaries).toHaveBeenCalledTimes(4);
  expect(f.state.reactionRemainder["account:friend"]).toEqual(
    targets.slice(256),
  );
});
it("continues pending projection turns without retaining unfinished rows", async () => {
  const f = fixture();
  summaries
    .mockResolvedValueOnce({ items: [], next: "p:work", pending: true })
    .mockResolvedValueOnce(page(id(1)));
  requestConversationReactions(f.set, f.get, conversation, [id(1)]);
  await vi.waitFor(() =>
    expect(f.state.reactions["account:friend"]?.[0]?.target).toBe(id(1)),
  );
  expect(summaries.mock.calls.map((call) => call[4])).toEqual([null, "p:work"]);
});
it("shares physical admission with the message inspector and replaces queued background demand", async () => {
  const finishes: Array<(value: ReactionSummaryPage) => void> = [];
  summaries.mockImplementation(
    () => new Promise((resolve) => finishes.push(resolve)),
  );
  const first = readReactionPage("host", conversation, [id(1)], null);
  const replaced = readReactionPage("host", conversation, [id(2)], null).catch(
    () => null,
  );
  const inspected = readReactionPage("host", conversation, [id(3)], null, true);
  expect(summaries).toHaveBeenCalledTimes(1);
  finishes[0](page(id(1)));
  await first;
  expect(await replaced).toBe(null);
  expect(summaries).toHaveBeenCalledTimes(2);
  finishes[1](page(id(3)));
  expect(await inspected).toEqual(page(id(3)));
});

it("charges retained emoji strings by bytes as well as row count", async () => {
  const f = fixture();
  const targets = Array.from({ length: 500 }, (_, number) => id(number));
  const emoji = "x".repeat(100_000);
  summaries.mockImplementation((_owner, _kind, _target, selected: string[]) =>
    Promise.resolve(page(selected[0], emoji)),
  );
  requestConversationReactions(f.set, f.get, conversation, targets);
  await vi.waitFor(() =>
    expect(f.state.reactions["account:friend"]).toHaveLength(3),
  );
  expect(
    f.state.reactions["account:friend"].reduce(
      (bytes, row) => bytes + row.emoji.length * 6 + 256,
      0,
    ),
  ).toBeLessThanOrEqual(2 * 1024 * 1024);
  expect(f.state.reactionRemainder["account:friend"]).toContain(targets[192]);
});

it("does not publish a replaced navigation's delayed summary", async () => {
  const f = fixture();
  let finish!: (value: ReactionSummaryPage) => void;
  summaries.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  requestConversationReactions(f.set, f.get, conversation, [id(1)]);
  f.state.activeRequest++;
  finish(page(id(1)));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(f.state.reactions["account:friend"]).toEqual([]);
});
