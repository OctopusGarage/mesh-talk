// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { ReceptionStatus } from "./ReceptionStatus";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, vars?: { count: number }) =>
      `${key}${vars ? `:${vars.count}` : ""}`,
  }),
}));
it("batches unread announcements and keeps earlier arrivals actionable without moving focus", async () => {
  vi.useFakeTimers();
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const jump = vi.fn();
  const props = {
    conversation: "group",
    unread: ["late"],
    earlier: ["late"],
    onJump: jump,
  };
  await act(async () => root.render(createElement(ReceptionStatus, props)));
  expect(host.querySelector('[role="status"]')?.textContent).toBe("");
  await act(async () =>
    root.render(
      createElement(ReceptionStatus, { ...props, unread: ["late", "live"] }),
    ),
  );
  await act(async () => vi.advanceTimersByTime(400));
  await act(async () =>
    root.render(
      createElement(ReceptionStatus, {
        ...props,
        unread: ["late", "live", "next"],
      }),
    ),
  );
  await act(async () => vi.advanceTimersByTime(250));
  expect(host.querySelector('[role="status"]')?.textContent).toBe(
    "conversation.unreadSummary:3",
  );
  // Reading and duplicate callbacks do not cause another spoken status.
  await act(async () =>
    root.render(createElement(ReceptionStatus, { ...props, unread: ["late"] })),
  );
  await act(async () => vi.advanceTimersByTime(650));
  expect(host.querySelector('[role="status"]')?.textContent).toBe(
    "conversation.unreadSummary:3",
  );
  const button = host.querySelector("button")!;
  await act(async () => button.click());
  expect(jump).toHaveBeenCalledWith("late");
  expect(document.activeElement).not.toBe(button);
  await act(async () => root.unmount());
  host.remove();
  vi.useRealTimers();
});

it("explains unavailable history and leaves clearing unread to the explicit action", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const host = document.createElement("div");
  const root = createRoot(host);
  const markRead = vi.fn();
  await act(async () =>
    root.render(
      createElement(ReceptionStatus, {
        conversation: "group",
        unread: ["pruned"],
        earlier: ["pruned"],
        onJump: vi.fn(),
        unavailable: true,
        onMarkRead: markRead,
      }),
    ),
  );
  expect(
    host.querySelector('[data-testid="unread-unavailable"]')?.textContent,
  ).toContain("conversation.unreadUnavailable");
  expect(markRead).not.toHaveBeenCalled();
  await act(async () =>
    (
      host.querySelector(
        '[data-testid="unread-unavailable"] button',
      ) as HTMLButtonElement
    ).click(),
  );
  expect(markRead).toHaveBeenCalledOnce();
  await act(async () => root.unmount());
});

it("announces a different message even when the unread total repeats", async () => {
  vi.useFakeTimers();
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const host = document.createElement("div");
  const root = createRoot(host);
  const props = { conversation: "group", earlier: [], onJump: vi.fn() };
  await act(async () =>
    root.render(createElement(ReceptionStatus, { ...props, unread: ["a"] })),
  );
  await act(async () => vi.advanceTimersByTime(650));
  const firstAnnouncement = host.querySelector(
    '[data-testid="reception-announcement"]',
  )!.firstChild;
  await act(async () =>
    root.render(createElement(ReceptionStatus, { ...props, unread: [] })),
  );
  await act(async () =>
    root.render(createElement(ReceptionStatus, { ...props, unread: ["b"] })),
  );
  await act(async () => vi.advanceTimersByTime(650));
  const secondAnnouncement = host.querySelector(
    '[data-testid="reception-announcement"]',
  )!.firstChild;
  expect(secondAnnouncement?.textContent).toBe("conversation.unreadSummary:1");
  expect(secondAnnouncement).not.toBe(firstAnnouncement);
  await act(async () => root.unmount());
  vi.useRealTimers();
});

it("presents a completed peer check as an observation rather than global sync", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const host = document.createElement("div");
  const root = createRoot(host);
  await act(async () =>
    root.render(
      createElement(ReceptionStatus, {
        conversation: "group",
        unread: [],
        earlier: [],
        onJump: vi.fn(),
        recovery: { phase: "idle", received: 0, checkedAt: 42 },
      }),
    ),
  );
  expect(
    host.querySelector('[data-testid="peer-check-observation"]')?.textContent,
  ).toContain("conversation.peerChecked");
  expect(host.textContent).not.toContain("up to date");
  await act(async () => root.unmount());
});
