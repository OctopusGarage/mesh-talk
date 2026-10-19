// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

const { state, dialog, input, api } = vi.hoisted(() => ({
  state: {
    hidden: {} as Record<string, unknown>,
    open: vi.fn(),
  },
  dialog: { onOpenChange: null as null | ((open: boolean) => void) },
  input: {
    onChange: null as null | ((event: { target: { value: string } }) => void),
  },
  api: {
    search: vi.fn(async () =>
      ["alice", "bob", "carol"].map((id) => ({
        is_channel: false,
        account_id: id,
        target: id,
        label: id,
        from_me: false,
        who: id,
        text: "term",
        wall_clock: 1,
      })),
    ),
  },
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
