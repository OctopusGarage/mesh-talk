// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

const { state, dialog, input, api } = vi.hoisted(() => ({
  state: {
    hidden: {} as Record<string, unknown>,
    ownerCurrent: true,
    open: vi.fn(),
  },
  dialog: { onOpenChange: null as null | ((open: boolean) => void) },
  input: {
    onChange: null as null | ((event: { target: { value: string } }) => void),
  },
  api: {
    searchRecent: vi.fn(async () => ({
      hits: ["alice", "bob", "carol"].map((id) => ({
        is_channel: false,
        account_id: id as string | null,
        target: id,
        label: id,
        from_me: false,
        who: id,
        text: "term",
        wall_clock: 1,
        reference: { kind: "account", target: id, message_id: id },
      })),
      coverage: {
        scope: "recent-window",
        per_conversation_limit: 500,
        result_limit: 200,
        truncated: false,
      },
    })),
    searchContext: vi.fn(),
  },
}));

vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({
    owner: "host",
    current: () => state.ownerCurrent,
  }),
}));
vi.mock("@/lib/api", () => ({ chat: api }));
vi.mock("@/store/chat", () => ({
  useChat: (
    selector: (s: {
      peers: unknown[];
      ready: boolean;
      open: typeof state.open;
    }) => unknown,
  ) => selector({ peers: [], ready: true, open: state.open }),
}));
vi.mock("@/store/contactPolicy", () => ({
  useContactPolicy: (
    selector: (s: {
      contacts: Record<string, unknown>;
      loaded: boolean;
    }) => unknown,
  ) => selector({ contacts: state.hidden, loaded: true }),
}));
vi.mock("@/components/ui/dialog", () => {
  const Wrap = ({ children }: { children: ReactNode }) =>
    createElement("div", null, children);
  return {
    Dialog: ({
      children,
      onOpenChange,
    }: {
      children: ReactNode;
      onOpenChange: (open: boolean) => void;
    }) => {
      dialog.onOpenChange = onOpenChange;
      return createElement("div", null, children);
    },
    DialogContent: Wrap,
    DialogHeader: Wrap,
    DialogTitle: Wrap,
    DialogTrigger: Wrap,
  };
});
vi.mock("@/components/ui/input", () => ({
  Input: ({ onChange, ...props }: { onChange: typeof input.onChange }) => {
    input.onChange = onChange;
    return createElement("input", { ...props, onChange: () => {} });
  },
}));

import { SearchDialog } from "./SearchDialog";

it("keeps keyboard selection inside results after contact visibility changes", async () => {
  vi.useFakeTimers();
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(SearchDialog)));
    await act(async () => dialog.onOpenChange?.(true));
    await act(async () => input.onChange?.({ target: { value: "term" } }));
    await act(async () => vi.advanceTimersByTimeAsync(250));
    const rows = container.querySelectorAll<HTMLElement>(
      '[data-testid="search-result"]',
    );
    expect(rows).toHaveLength(3);
    await act(async () =>
      rows[2].dispatchEvent(new MouseEvent("mouseover", { bubbles: true })),
    );
    expect(
      container
        .querySelector('[role="combobox"]')
        ?.getAttribute("aria-activedescendant"),
    ).toBe("search-result-2");

    state.hidden = { bob: {}, carol: {} };
    await act(async () => root.render(createElement(SearchDialog)));
    expect(
      container.querySelectorAll('[data-testid="search-result"]'),
    ).toHaveLength(1);
    expect(
      container
        .querySelector('[role="combobox"]')
        ?.getAttribute("aria-activedescendant"),
    ).toBe("search-result-0");
    expect(
      container
        .querySelector('[data-testid="search-result"]')
        ?.getAttribute("aria-selected"),
    ).toBe("true");
  } finally {
    await act(async () => root.unmount());
    vi.useRealTimers();
    state.hidden = {};
  }
});

it("opens duplicate results with exact references and refuses unavailable context", async () => {
  vi.useFakeTimers();
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    api.searchRecent.mockResolvedValueOnce({
      hits: ["one", "two"].map((id) => ({
        is_channel: false,
        account_id: "alice",
        target: "alice",
        label: "Alice",
        from_me: false,
        who: "Alice",
        text: "same",
        wall_clock: 1,
        reference: { kind: "account", target: "alice", message_id: id },
      })),
      coverage: {
        scope: "recent-window",
        per_conversation_limit: 500,
        result_limit: 200,
        truncated: true,
      },
    });
    api.searchContext.mockResolvedValueOnce({
      status: "found",
      reference: { kind: "account", target: "alice", message_id: "two" },
      messages: [
        {
          id: "two",
          text: "same",
          wall_clock: 1,
          from_me: false,
          who: "Alice",
        },
      ],
    });
    state.open.mockClear();
    await act(async () => root.render(createElement(SearchDialog)));
    await act(async () => dialog.onOpenChange?.(true));
    await act(async () => input.onChange?.({ target: { value: "same" } }));
    await act(async () => vi.advanceTimersByTimeAsync(250));
    expect(api.searchRecent).toHaveBeenCalledWith("host", "same");
    await act(async () =>
      container
        .querySelectorAll<HTMLElement>('[data-testid="search-result"]')[1]
        .click(),
    );
    expect(api.searchContext).toHaveBeenLastCalledWith("host", {
      kind: "account",
      target: "alice",
      message_id: "two",
    });
    expect(state.open).toHaveBeenCalledWith(
      { kind: "account", id: "alice", name: "Alice" },
      expect.objectContaining({
        reference: { kind: "account", target: "alice", message_id: "two" },
      }),
    );

    await act(async () => dialog.onOpenChange?.(true));
    await act(async () => vi.advanceTimersByTimeAsync(250));
    api.searchContext.mockResolvedValueOnce({
      status: "unavailable",
      messages: [],
    });
    state.open.mockClear();
    await act(async () =>
      container
        .querySelector<HTMLElement>('[data-testid="search-result"]')
        ?.click(),
    );
    expect(state.open).not.toHaveBeenCalled();
    expect(container.querySelector('[role="alert"]')).not.toBeNull();
  } finally {
    await act(async () => root.unmount());
    vi.useRealTimers();
  }
});

it.each(["retired-owner", "closed-dialog", "hidden-contact"])(
  "discards a context response after %s",
  async (retirement) => {
    vi.useFakeTimers();
    const container = document.createElement("div");
    const root = createRoot(container);
    let resolveContext!: (value: unknown) => void;
    api.searchContext.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveContext = resolve;
        }),
    );
    state.open.mockClear();
    try {
      await act(async () => root.render(createElement(SearchDialog)));
      await act(async () => dialog.onOpenChange?.(true));
      await act(async () => input.onChange?.({ target: { value: "term" } }));
      await act(async () => vi.advanceTimersByTimeAsync(250));
      await act(async () =>
        container
          .querySelector<HTMLElement>('[data-testid="search-result"]')
          ?.click(),
      );
      if (retirement === "retired-owner") state.ownerCurrent = false;
      else if (retirement === "closed-dialog")
        await act(async () => dialog.onOpenChange?.(false));
      else {
        state.hidden = { alice: {} };
        await act(async () => root.render(createElement(SearchDialog)));
      }
      await act(async () =>
        resolveContext({
          status: "found",
          reference: { kind: "account", target: "alice", message_id: "alice" },
          messages: [
            { id: "alice", text: "term", from_me: false, wall_clock: 1 },
          ],
        }),
      );
      expect(state.open).not.toHaveBeenCalled();
    } finally {
      state.ownerCurrent = true;
      state.hidden = {};
      await act(async () => root.unmount());
      vi.useRealTimers();
    }
  },
);

it("does not reinterpret an unbound legacy device as an account", async () => {
  vi.useFakeTimers();
  const container = document.createElement("div");
  const root = createRoot(container);
  api.searchRecent.mockResolvedValueOnce({
    hits: [
      {
        is_channel: false,
        account_id: null,
        target: "device",
        label: "Legacy",
        from_me: false,
        who: "Legacy",
        text: "term",
        wall_clock: 1,
        reference: {
          kind: "legacy-device",
          target: "device",
          message_id: "id",
        },
      },
    ],
    coverage: {
      scope: "recent-window",
      per_conversation_limit: 500,
      result_limit: 200,
      truncated: false,
    },
  });
  state.open.mockClear();
  try {
    await act(async () => root.render(createElement(SearchDialog)));
    await act(async () => dialog.onOpenChange?.(true));
    await act(async () => input.onChange?.({ target: { value: "term" } }));
    await act(async () => vi.advanceTimersByTimeAsync(250));
    await act(async () =>
      container
        .querySelector<HTMLElement>('[data-testid="search-result"]')
        ?.click(),
    );
    expect(state.open).not.toHaveBeenCalled();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "legacy conversation",
    );
  } finally {
    await act(async () => root.unmount());
    vi.useRealTimers();
  }
});

it("keeps the latest query when an older search completes afterward", async () => {
  vi.useFakeTimers();
  const container = document.createElement("div");
  const root = createRoot(container);
  const response = (name: string) => ({
    hits: [
      {
        is_channel: false,
        account_id: name,
        target: name,
        label: name,
        from_me: false,
        who: name,
        text: name,
        wall_clock: 1,
        reference: { kind: "account", target: name, message_id: name },
      },
    ],
    coverage: {
      scope: "recent-window",
      per_conversation_limit: 500,
      result_limit: 200,
      truncated: false,
    },
  });
  let first!: (value: ReturnType<typeof response>) => void;
  let second!: (value: ReturnType<typeof response>) => void;
  api.searchRecent.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        first = resolve;
      }),
  );
  api.searchRecent.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        second = resolve;
      }),
  );
  try {
    await act(async () => root.render(createElement(SearchDialog)));
    await act(async () => dialog.onOpenChange?.(true));
    await act(async () => input.onChange?.({ target: { value: "first" } }));
    await act(async () => vi.advanceTimersByTimeAsync(250));
    await act(async () => input.onChange?.({ target: { value: "second" } }));
    await act(async () => vi.advanceTimersByTimeAsync(250));
    await act(async () => second(response("latest")));
    expect(
      container.querySelector('[data-testid="search-result"]')?.textContent,
    ).toContain("latest");
    await act(async () => first(response("outdated")));
    expect(
      container.querySelector('[data-testid="search-result"]')?.textContent,
    ).not.toContain("outdated");
  } finally {
    await act(async () => root.unmount());
    vi.useRealTimers();
  }
});

it.each(["missing", "recalled", "expired", "wrong-namespace"])(
  "rejects a found context whose exact target is %s",
  async (failure) => {
    vi.useFakeTimers();
    const container = document.createElement("div");
    const root = createRoot(container);
    api.searchContext.mockResolvedValueOnce({
      status: "found",
      reference: {
        kind: failure === "wrong-namespace" ? "legacy-device" : "account",
        target: "alice",
        message_id: "alice",
      },
      messages: [
        {
          id: failure === "missing" ? "other" : "alice",
          text: "term",
          from_me: false,
          wall_clock: 1,
          recalled: failure === "recalled",
          expires_at_ms: failure === "expired" ? 1 : null,
        },
      ],
    });
    state.open.mockClear();
    try {
      await act(async () => root.render(createElement(SearchDialog)));
      await act(async () => dialog.onOpenChange?.(true));
      await act(async () => input.onChange?.({ target: { value: "term" } }));
      await act(async () => vi.advanceTimersByTimeAsync(250));
      await act(async () =>
        container
          .querySelector<HTMLElement>('[data-testid="search-result"]')
          ?.click(),
      );
      expect(state.open).not.toHaveBeenCalled();
      expect(container.querySelector('[role="alert"]')?.textContent).toContain(
        "no longer available",
      );
    } finally {
      await act(async () => root.unmount());
      vi.useRealTimers();
    }
  },
);
