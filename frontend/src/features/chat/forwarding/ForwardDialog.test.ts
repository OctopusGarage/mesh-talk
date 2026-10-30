// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ChannelInfo } from "@/lib/types";
import type { ForwardResult } from "@/lib/api/messageForwarding";

const fixture = vi.hoisted(() => ({
  revision: 1,
  accounts: [{ account_id: "bob", names: ["Bob"] }],
  channels: [] as ChannelInfo[],
  favorites: {},
  inspect: vi.fn(),
  list: vi.fn(),
  send: vi.fn(),
}));
vi.mock("@/store/chat", () => ({
  useChat: (select: (state: typeof fixture) => unknown) => select(fixture),
  displayName: (_favorites: unknown, _id: string, name: string) => name,
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => {
    const revision = fixture.revision;
    return { owner: "owner", current: () => fixture.revision === revision };
  },
}));
vi.mock("@/lib/api/messageForwarding", () => ({
  messageForwarding: fixture,
}));
vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({ children }: { children: React.ReactNode }) => children,
  DialogContent: ({ children }: { children: React.ReactNode }) =>
    createElement("div", { "data-testid": "forward-dialog" }, children),
  DialogHeader: ({ children }: { children: React.ReactNode }) => children,
  DialogTitle: ({ children }: { children: React.ReactNode }) => children,
  DialogDescription: ({ children }: { children: React.ReactNode }) => children,
}));
import { ForwardDialog } from "./ForwardDialog";

const source = {
  kind: "account" as const,
  target: "alice",
  message_ids: ["source"],
};
const request = {
  operation_id: "original-operation",
  source,
  destinations: [{ kind: "channel" as const, target: "group" }],
  mode: "individual" as const,
};
function batch(status: "pending" | "accepted" | "failed" = "pending") {
  return {
    request,
    result: {
      operation_id: request.operation_id,
      items: [
        {
          target: request.destinations[0],
          source_id: "source",
          message_id: status === "accepted" ? "public-event" : null,
          status,
          error: status === "failed" ? "Route unavailable" : null,
        },
      ],
    },
  };
}
let host: HTMLDivElement;
let root: Root;
let accepted: (result: ForwardResult) => void;
beforeEach(() => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  vi.useFakeTimers();
  fixture.revision = 1;
  fixture.channels = [];
  fixture.inspect.mockReset().mockResolvedValue({
    messages: [{ id: "source", text: "Canonical preview" }],
  });
  fixture.list.mockReset().mockResolvedValue([]);
  fixture.send.mockReset().mockImplementation((_owner, sent) => ({
    operation_id: sent.operation_id,
    items: [{ ...batch().result.items[0], target: sent.destinations[0] }],
  }));
  accepted = vi.fn<(result: ForwardResult) => void>();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.useRealTimers();
});
async function render() {
  await act(async () =>
    root.render(
      createElement(ForwardDialog, {
        source,
        onClose: vi.fn(),
        onAccepted: accepted,
      }),
    ),
  );
}
async function click(label: string) {
  const button = [...host.querySelectorAll("button")].find((button) =>
    button.textContent?.includes(label),
  );
  expect(button, `button ${label} must be available`).toBeDefined();
  await act(async () => button!.click());
}

it.each(["member", "owner"] as const)(
  "offers a certified reference-only %s target while excluding inactive account roles",
  async (role) => {
    fixture.channels = [role, "unknown", "invited", "left", "dissolved"].map(
      (value) => ({
        channel_id: value,
        name: `Group ${value}`,
        owner: "sibling",
        member_count: 1,
        shared_reference: true,
        account_access: {
          account_id: "self",
          role: value,
          execution: { kind: "waiting_for_device" },
        },
      }),
    ) as ChannelInfo[];
    await render();
    expect(host.textContent).toContain(`Group ${role}`);
    for (const unavailable of ["unknown", "invited", "left", "dissolved"])
      expect(host.textContent).not.toContain(`Group ${unavailable}`);
  },
);

it("retains pending status and observes its exact batch without another submission", async () => {
  await render();
  await act(async () =>
    host.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click(),
  );
  await click("Confirm and send");
  expect(host.textContent).toContain("Waiting for a linked device");
  expect(host.textContent).toContain("1 pending");
  const sent = fixture.send.mock.calls[0][1];
  const committed = batch("accepted");
  committed.request = sent;
  committed.result.operation_id = sent.operation_id;
  committed.result.items[0].target = sent.destinations[0];
  fixture.list.mockResolvedValue([batch("accepted")]);
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(host.textContent).not.toContain("Accepted for delivery");
  fixture.list.mockResolvedValue([committed]);
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(host.textContent).toContain("Accepted for delivery");
  expect(fixture.send).toHaveBeenCalledTimes(1);
  expect(accepted).toHaveBeenLastCalledWith(committed.result);
});

it("observes an admitted batch after its send response is lost without submitting or inspecting again", async () => {
  await render();
  let retained = batch();
  fixture.send.mockImplementationOnce(async (_owner, sent) => {
    retained = batch();
    retained.request = sent;
    retained.result.operation_id = sent.operation_id;
    retained.result.items[0].target = sent.destinations[0];
    fixture.list.mockResolvedValue([retained]);
    throw new Error("Response lost after admission");
  });
  await act(async () =>
    host.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click(),
  );
  await click("Confirm and send");
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(host.textContent).toContain("Waiting for a linked device");
  const committed = {
    ...retained,
    result: {
      ...retained.result,
      items: retained.result.items.map((item) => ({
        ...item,
        status: "accepted" as const,
        message_id: "public-event",
      })),
    },
  };
  fixture.list.mockResolvedValue([committed]);
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(host.textContent).toContain("Accepted for delivery");
  expect(fixture.send).toHaveBeenCalledTimes(1);
  expect(fixture.inspect).toHaveBeenCalledTimes(1);
  expect(accepted).toHaveBeenLastCalledWith(committed.result);
});

it("reopens pending work and observes completion even after source inspection fails", async () => {
  fixture.inspect.mockRejectedValue(new Error("Source was recalled"));
  fixture.list.mockResolvedValue([batch()]);
  await render();
  await click("Review 1");
  expect(host.textContent).toContain("Waiting for a linked device");
  fixture.list.mockResolvedValue([batch("accepted")]);
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(host.textContent).toContain("Accepted for delivery");
  expect(fixture.send).not.toHaveBeenCalled();
});

it("retries partial failures with the original batch request while retaining pending siblings", async () => {
  const mixed = batch();
  mixed.request = {
    ...request,
    source: {
      ...source,
      message_ids: ["source", "failed-source", "accepted-source"],
    },
  };
  mixed.result.items.push({
    ...batch("failed").result.items[0],
    source_id: "failed-source",
  });
  mixed.result.items.push({
    ...batch("accepted").result.items[0],
    source_id: "accepted-source",
  });
  fixture.list.mockResolvedValue([mixed]);
  fixture.send.mockResolvedValue(mixed.result);
  await render();
  await click("Review 2");
  await click("Retry unfinished items");
  expect(fixture.send).toHaveBeenCalledExactlyOnceWith("owner", mixed.request);
  expect(host.textContent).toContain("Waiting for a linked device");
  expect(host.textContent).toContain("Accepted for delivery");
  expect(host.textContent).toContain("Route unavailable");
});

it("does not overlap pending reads or publish a retired owner's completion", async () => {
  fixture.list.mockResolvedValue([batch()]);
  await render();
  await click("Review 1");
  let resolve!: (value: ReturnType<typeof batch>[]) => void;
  fixture.list.mockImplementationOnce(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  const calls = fixture.list.mock.calls.length;
  await act(async () => vi.advanceTimersByTimeAsync(4000));
  expect(fixture.list).toHaveBeenCalledTimes(calls);
  fixture.revision++;
  await act(async () => resolve([batch("accepted")]));
  expect(accepted).not.toHaveBeenCalled();
  expect(host.textContent).not.toContain("Accepted for delivery");
});
