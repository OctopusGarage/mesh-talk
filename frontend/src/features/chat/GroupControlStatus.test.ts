// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import type { AccountGroupOperationStatus, ChannelInfo } from "@/lib/types";
import type { GroupOperationTicket } from "@/store/accountGroupOperations";
const { state } = vi.hoisted(() => ({
  state: {
    myAccountId: "account",
    groupOperations: {} as Record<string, GroupOperationTicket>,
    channels: [] as ChannelInfo[],
    refreshGroupOperations: vi.fn(async () => {}),
    cancelGroupOperation: vi.fn(async () => ({ kind: "pending" })),
  },
}));
vi.mock("@/store/chat", () => ({
  useChat: (select: (snapshot: typeof state) => unknown) => select(state),
}));
import { GroupControlStatus, PendingGroupControls } from "./GroupControlStatus";
function status(
  phase: AccountGroupOperationStatus["phase"],
): AccountGroupOperationStatus {
  return {
    id: "original",
    account_id: "account",
    group: "group",
    kind: "rename",
    phase,
    executions: [],
    committed_events: [],
    primary_event: null,
  };
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  state.groupOperations = {};
  state.channels = [];
  state.refreshGroupOperations.mockClear();
  state.cancelGroupOperation.mockClear();
});
it("checks the existing operation and cancels its original ID without resubmitting", async () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    await act(async () =>
      root.render(
        createElement(GroupControlStatus, {
          outcome: {
            kind: "pending",
            id: "original",
            status: status({ kind: "waiting_for_executor" }),
          },
        }),
      ),
    );
    const refresh = [...host.querySelectorAll("button")].find(
      (button) => button.textContent === "Check status",
    );
    expect(refresh).toBeDefined();
    await act(async () => refresh!.click());
    expect(state.refreshGroupOperations).toHaveBeenCalledOnce();
    await act(async () =>
      [...host.querySelectorAll("button")]
        .find((button) => button.textContent === "Request cancellation")!
        .click(),
    );
    expect(state.cancelGroupOperation).toHaveBeenCalledExactlyOnceWith(
      "original",
    );
  } finally {
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
  }
});
it("renders linked-device upgrades as pending rather than success", () => {
  state.channels = [
    {
      channel_id: "group",
      name: "Group",
      owner: "source",
      member_count: 2,
      account_access: {
        account_id: "account",
        role: "owner",
        execution: { kind: "requires_upgrade" },
      },
    },
  ];
  const html = renderToStaticMarkup(
    createElement(GroupControlStatus, {
      outcome: {
        kind: "pending",
        id: "original",
        status: status({ kind: "waiting_for_executor" }),
      },
    }),
  );
  expect(html).toContain("Update a linked device");
  expect(html).toContain('role="status"');
});
it.each(["committed", "cancelled"] as const)(
  "shows %s as an observed terminal result",
  (kind) => {
    const html = renderToStaticMarkup(
      createElement(GroupControlStatus, {
        outcome: { kind, id: "original", status: status({ kind }) },
      }),
    );
    expect(html).toContain(kind === "committed" ? "completed" : "cancelled");
    expect(html).not.toContain("Request cancellation");
  },
);
it("keeps background operation presentation bounded and excludes other accounts and message bodies", () => {
  state.groupOperations = Object.fromEntries(
    Array.from({ length: 20 }, (_, index) => {
      const id = `op-${index}`;
      return [
        id,
        {
          id,
          account_id: "account",
          group: "group",
          kind: "rename",
          stage: "observed",
          observation: index,
          status: { ...status({ kind: "accepted" }), id },
        },
      ];
    }),
  );
  state.groupOperations.foreign = {
    ...state.groupOperations["op-0"],
    id: "foreign",
    account_id: "other",
  };
  const html = renderToStaticMarkup(
    createElement(PendingGroupControls, { group: "group" }),
  );
  expect(
    (html.match(/Request cancellation/g) ?? []).length,
  ).toBeLessThanOrEqual(8);
  expect(html).not.toContain("foreign");
});

it.each(["reaction", "recall"] as const)(
  "names pending %s operations in the existing status list",
  (kind) => {
    state.groupOperations = {
      original: {
        id: "original",
        account_id: "account",
        group: "group",
        kind,
        stage: "observed",
        observation: 1,
        status: { ...status({ kind: "accepted" }), kind },
      },
    };
    const html = renderToStaticMarkup(
      createElement(PendingGroupControls, { group: "group" }),
    );
    expect(html).toContain(
      kind === "reaction" ? "Update reaction" : "Recall message",
    );
    expect(html).toContain("Request cancellation");
    expect(html).not.toContain("completed");
  },
);

it.each([undefined, "accepted", "cancelled", "committed", "rejected"] as const)(
  "keeps File check/cancel reachable only while nonterminal: %s",
  (phase) => {
    state.groupOperations = {
      file: {
        id: "file",
        account_id: "account",
        group: "group",
        kind: "file",
        stage: "unconfirmed",
        observation: 1,
        ...(phase
          ? {
              status: {
                ...status(
                  phase === "rejected"
                    ? { kind: phase, reason: "not_authorized" }
                    : { kind: phase },
                ),
                id: "file",
                kind: "file",
              },
            }
          : {}),
      },
    };
    const html = renderToStaticMarkup(
      createElement(PendingGroupControls, { group: "group" }),
    );
    if (!phase || phase === "accepted") {
      expect(html).toContain("Attachments");
      expect(html).toContain("Request cancellation");
    } else expect(html).toBe("");
  },
);
