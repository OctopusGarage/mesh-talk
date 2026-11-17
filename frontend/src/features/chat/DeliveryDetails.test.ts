// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot as createReactRoot } from "react-dom/client";
import { DeliveryRecoveryProvider } from "./DeliveryRecoveryProvider";
import { expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { DeliveryFooter } from "./DeliveryFooter";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import type { ChatMessage } from "@/store/conversationModel";

function createRoot(host: HTMLElement) {
  const root = createReactRoot(host);
  return {
    render: (children: ReactNode) =>
      root.render(createElement(DeliveryRecoveryProvider, { children })),
    unmount: () => root.unmount(),
  };
}

const base: ChatMessage = {
  id: "accepted",
  fromMe: true,
  who: "me",
  text: "text",
  wallClock: 1,
  replyTo: null,
  delivery: "awaiting",
  deliveryObservation: { kind: "attempting", ageMs: 3000, sampledAt: 100 },
};

it("discloses member and post-office confirmations independently as status changes", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  await i18n.changeLanguage("en");
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const show = (postOfficeCopies: number) =>
    act(async () =>
      root.render(
        createElement(DeliveryFooter, {
          message: { ...base, replicas: 1, postOfficeCopies },
          isChannel: true,
        }),
      ),
    );
  try {
    await show(0);
    expect(host.querySelectorAll("button[aria-expanded]")).toHaveLength(1);
    await show(1);
    const buttons = host.querySelectorAll<HTMLButtonElement>(
      "button[aria-expanded]",
    );
    expect(buttons).toHaveLength(2);
    expect(buttons[0].getAttribute("aria-label")).toBe(
      "Last confirmed: 1 member copy",
    );
    expect(buttons[1].getAttribute("aria-label")).toBe(
      "Post office confirmed a stored copy",
    );
    for (const button of buttons) {
      await act(async () => button.click());
      const panel = document.getElementById(
        button.getAttribute("aria-controls")!,
      );
      expect(panel?.textContent).toContain(button.getAttribute("aria-label"));
      expect(panel?.textContent).toContain(
        button === buttons[0]
          ? "not a delivery or read receipt"
          : "Recipient delivery, downloads and reading are separate.",
      );
    }
    await act(async () =>
      buttons[1].dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
    );
    expect(buttons[0].getAttribute("aria-expanded")).toBe("true");
    expect(buttons[1].getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(buttons[1]);
    await show(0);
    expect(host.querySelectorAll("button[aria-expanded]")).toHaveLength(1);
    expect(host.textContent).not.toContain(
      "Post office confirmed a stored copy",
    );
    expect(host.textContent).toContain("Last confirmed: 1 member copy");
  } finally {
    await act(async () => root.unmount());
    host.remove();
  }
});

it("keeps sticker receipts compact and accessible while retaining delivery details", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  await i18n.changeLanguage("en");
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () =>
    root.render(
      createElement(DeliveryFooter, {
        message: base,
        isChannel: false,
      }),
    ),
  );
  const button = host.querySelector<HTMLButtonElement>(
    '[data-testid="delivery-details-trigger"]',
  )!;
  expect(button.getAttribute("aria-label")).toBe("Awaiting receipt");
  expect(button.querySelector(".sr-only")?.textContent).toBe(
    "Awaiting receipt",
  );
  await act(async () => button.click());
  expect(
    document.querySelector('[data-testid="delivery-details"]')?.textContent,
  ).toContain("Awaiting receipt");
  await act(async () => root.unmount());
  host.remove();
});
it("opens local past-tense facts with monotonic age, accessible description, and Escape focus restoration", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  await i18n.changeLanguage("en");
  const clock = vi.spyOn(performance, "now").mockReturnValue(2100);
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () =>
    root.render(
      createElement(DeliveryFooter, { message: base, isChannel: false }),
    ),
  );
  const button = host.querySelector<HTMLButtonElement>(
    'button[data-testid="delivery-details-trigger"]',
  )!;
  expect(button).not.toBeNull();
  button.focus();
  await act(async () => button.click());
  let popup = document.querySelector('[data-testid="delivery-details"]')!;
  expect(popup.textContent).toContain("Local attempt started 5 seconds ago.");
  expect(popup.textContent).toContain(
    "Delivery retries automatically; you do not need to resend this message.",
  );
  expect(popup.getAttribute("aria-labelledby")).toBeTruthy();
  expect(popup.getAttribute("aria-describedby")).toBeTruthy();
  expect(popup.querySelector("[aria-live]")).toBeNull();
  await act(async () =>
    document.activeElement?.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  expect(document.querySelector('[data-testid="delivery-details"]')).toBeNull();
  await act(async () => new Promise((resolve) => setTimeout(resolve, 10)));
  expect(document.activeElement).toBe(button);
  clock.mockReturnValue(120100);
  await act(async () => button.click());
  popup = document.querySelector('[data-testid="delivery-details"]')!;
  expect(popup.textContent).toContain("2 minutes ago");
  await act(async () =>
    root.render(
      createElement(DeliveryFooter, {
        message: { ...base, delivery: "delivered" },
        isChannel: false,
      }),
    ),
  );
  expect(document.querySelector('[data-testid="delivery-details"]')).toBeNull();
  await act(async () => root.unmount());
  host.remove();
  clock.mockRestore();
});

it("uses explicit fallback for missing or malformed facts and closes across message, session and navigation changes", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  for (const observation of [
    undefined,
    { kind: "future", ageMs: 0, sampledAt: 0 },
    { kind: "attempting", ageMs: -1, sampledAt: 0 },
  ]) {
    await act(async () =>
      root.render(
        createElement(DeliveryFooter, {
          message: {
            ...base,
            deliveryObservation:
              observation as ChatMessage["deliveryObservation"],
          },
          isChannel: false,
        }),
      ),
    );
    expect(
      host.querySelector('[data-testid="delivery-details-trigger"]'),
    ).not.toBeNull();
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>(
          '[data-testid="delivery-details-trigger"]',
        )!
        .click(),
    );
    expect(
      document.querySelector('[data-testid="delivery-details"]')?.textContent,
    ).toContain("No current local attempt details are available.");
    await act(async () =>
      root.render(
        createElement(DeliveryFooter, {
          message: { ...base, id: "replacement" },
          isChannel: false,
        }),
      ),
    );
    expect(
      document.querySelector('[data-testid="delivery-details"]'),
    ).toBeNull();
  }
  for (const invalidate of [
    () => useAuth.setState((s) => ({ generation: s.generation + 1 })),
    () =>
      useChat.setState({
        active: { kind: "account", id: "other", name: "Other" },
      }),
    () => useChat.setState((s) => ({ runEpoch: s.runEpoch + 1 })),
  ]) {
    expect(
      host.querySelector('[data-testid="delivery-details-trigger"]'),
    ).not.toBeNull();
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>(
          '[data-testid="delivery-details-trigger"]',
        )!
        .click(),
    );
    expect(
      document.querySelector('[data-testid="delivery-details"]'),
    ).not.toBeNull();
    await act(async () => invalidate());
    expect(
      document.querySelector('[data-testid="delivery-details"]'),
    ).toBeNull();
  }
  await act(async () => root.unmount());
  host.remove();
});

it("discloses retry history and file-card receipt meaning without changing attachment progress", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const clock = vi.spyOn(performance, "now").mockReturnValue(60000);
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  for (const variant of [
    { sticker: "pack:sticker" },
    {
      file: {
        name: "file",
        size: 100,
        mime: "text/plain",
        fileConv: "file-id",
        media: false,
      },
    },
  ]) {
    await act(async () =>
      root.render(
        createElement(DeliveryFooter, {
          message: {
            ...base,
            ...variant,
            deliveryObservation: {
              kind: "retry_scheduled",
              ageMs: 120000,
              sampledAt: 0,
            },
          },
          isChannel: false,
        }),
      ),
    );
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>(
          '[data-testid="delivery-details-trigger"]',
        )!
        .click(),
    );
    const popup = document.querySelector('[data-testid="delivery-details"]')!;
    expect(popup.textContent).toContain("Retry was scheduled 3 minutes ago.");
    expect(popup.textContent).not.toMatch(
      /offline|failed|retry in|sending now/i,
    );
    if ("file" in variant)
      expect(popup.textContent).toContain(
        "A receipt confirms the file card. File download and reading are separate.",
      );
    expect(popup.textContent).not.toContain("File card delivered");
    await act(async () =>
      root.render(
        createElement(DeliveryFooter, {
          message: { ...base, delivery: "delivered" },
          isChannel: false,
        }),
      ),
    );
  }
  await act(async () => root.unmount());
  host.remove();
  clock.mockRestore();
});

vi.mock("./SettingsDialog", () => ({
  SettingsDialog: ({
    open,
    initialTarget,
    onOpenChange,
    onCloseAutoFocus,
  }: {
    open: boolean;
    initialTarget: { category: string; controlId: string };
    onOpenChange: (open: boolean) => void;
    onCloseAutoFocus: (event: Event) => void;
  }) =>
    open
      ? createElement(
          "div",
          { "data-testid": "recovery-settings" },
          `${initialTarget.category}:${initialTarget.controlId}`,
          createElement(
            "button",
            {
              onClick: () => {
                onCloseAutoFocus(new Event("close", { cancelable: true }));
                onOpenChange(false);
              },
            },
            "Close recovery",
          ),
        )
      : null,
}));
vi.mock("./DiagnosticsDialog", () => ({
  DiagnosticsDialog: ({
    open,
    onOpenChange,
    onCloseAutoFocus,
  }: {
    open: boolean;
    onOpenChange: (open: boolean) => void;
    onCloseAutoFocus: (event: Event) => void;
  }) =>
    open
      ? createElement(
          "div",
          { "data-testid": "recovery-diagnostics" },
          createElement(
            "button",
            {
              onClick: () => {
                onCloseAutoFocus(new Event("close", { cancelable: true }));
                onOpenChange(false);
              },
            },
            "Close recovery",
          ),
        )
      : null,
}));

it("offers evidence-specific recovery guidance and safe connection navigation", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  await i18n.changeLanguage("en");
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  for (const [reason, text, action, target] of [
    [
      "post_office_disabled",
      "Post-office sending was disabled",
      "Review delivery settings",
      "recovery-settings",
    ],
    [
      "no_known_route",
      "No eligible route was known",
      "Open connection diagnostics",
      "recovery-diagnostics",
    ],
    [
      "attempt_timed_out",
      "The local attempt timed out",
      "Open connection diagnostics",
      "recovery-diagnostics",
    ],
  ]) {
    await act(async () =>
      root.render(
        createElement(DeliveryFooter, {
          message: {
            ...base,
            id: reason,
            deliveryObservation: {
              kind: "retry_scheduled",
              ageMs: 0,
              sampledAt: performance.now(),
              reason: reason as NonNullable<
                ChatMessage["deliveryObservation"]
              >["reason"],
            },
          },
          isChannel: false,
        }),
      ),
    );
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>(
          '[data-testid="delivery-details-trigger"]',
        )!
        .click(),
    );
    const popup = document.querySelector('[data-testid="delivery-details"]')!;
    expect(popup.textContent).toContain(text);
    expect(popup.textContent).not.toMatch(
      /recipient is offline|delivery failed/i,
    );
    const button = Array.from(popup.querySelectorAll("button")).find(
      (b) => b.textContent === action,
    )!;
    expect(button).toBeTruthy();
    await act(async () => button.click());
    expect(
      document.querySelector('[data-testid="delivery-details"]'),
    ).toBeNull();
    expect(document.querySelector(`[data-testid="${target}"]`)).not.toBeNull();
    if (target === "recovery-settings")
      expect(
        document.querySelector(`[data-testid="${target}"]`)?.textContent,
      ).toContain("connection:setting-post-office");
    await act(async () =>
      document
        .querySelector<HTMLButtonElement>(`[data-testid="${target}"] button`)!
        .click(),
    );
    expect(document.activeElement).toBe(
      host.querySelector('[data-testid="delivery-details-trigger"]'),
    );
  }
  await act(async () => root.unmount());
  host.remove();
});

it("keeps recovery settings open after recipient confirmation removes the disclosure", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  await i18n.changeLanguage("en");
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const message = {
    ...base,
    deliveryObservation: {
      ...base.deliveryObservation!,
      reason: "post_office_disabled" as const,
    },
  };
  await act(async () =>
    root.render(createElement(DeliveryFooter, { message, isChannel: false })),
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="delivery-details-trigger"]',
      )!
      .click(),
  );
  await act(async () =>
    Array.from(
      document.querySelectorAll<HTMLButtonElement>(
        '[data-testid="delivery-details"] button',
      ),
    )
      .find((b) => b.textContent === "Review delivery settings")!
      .click(),
  );
  expect(
    document.querySelector('[data-testid="recovery-settings"]'),
  ).not.toBeNull();
  await act(async () =>
    root.render(
      createElement(DeliveryFooter, {
        message: { ...message, delivery: "delivered" },
        isChannel: false,
      }),
    ),
  );
  expect(
    document.querySelector('[data-testid="recovery-settings"]'),
  ).not.toBeNull();
  await act(async () => root.render(null));
  expect(
    document.querySelector('[data-testid="recovery-settings"]'),
  ).not.toBeNull();
  await act(async () =>
    useAuth.setState((state) => ({ generation: state.generation + 1 })),
  );
  expect(
    document.querySelector('[data-testid="recovery-settings"]'),
  ).toBeNull();
  await act(async () => root.unmount());
  host.remove();
});

it("keeps copy and delivered states icon-only until explicitly expanded inline", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  await i18n.changeLanguage("en");
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const variants = [
    {
      message: { ...base, replicas: undefined },
      isChannel: true,
      label: "Copy status unavailable",
      detail: "Another linked device may have newer information.",
      count: "?",
    },
    {
      message: { ...base, replicas: 0 },
      isChannel: true,
      label: "No member copy confirmed",
      detail: "not a delivery or read receipt",
      count: "",
    },
    {
      message: { ...base, replicas: 1 },
      isChannel: true,
      label: "Last confirmed: 1 member copy",
      detail: "not a delivery or read receipt",
      count: "1",
    },
    {
      message: { ...base, replicas: 8 },
      isChannel: true,
      label: "Last confirmed: 2+ member copies",
      detail: "not a delivery or read receipt",
      count: "2+",
    },
    {
      message: { ...base, replicas: 0, postOfficeCopies: 1 },
      isChannel: true,
      label: "Post office confirmed a stored copy",
      detail: "Recipient delivery, downloads and reading are separate.",
      count: "",
    },
    {
      message: { ...base, delivery: "delivered" as const },
      isChannel: false,
      label: "Delivered",
      detail: "Not a read receipt.",
      count: "",
    },
  ];
  for (const [index, variant] of variants.entries()) {
    await act(async () =>
      root.render(
        createElement(DeliveryFooter, {
          message: { ...variant.message, id: `status-${index}` },
          isChannel: variant.isChannel,
        }),
      ),
    );
    const button = host.querySelector<HTMLButtonElement>(
      "button[aria-expanded]",
    )!;
    expect(button).not.toBeNull();
    expect(button.getAttribute("aria-label")).toBe(variant.label);
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(button.getAttribute("title")).toBeNull();
    expect(host.querySelector('[role="tooltip"]')).toBeNull();
    expect(host.textContent).not.toContain(variant.detail);
    expect(button.querySelector("svg")).not.toBeNull();
    expect(button.querySelector("[data-status-count]")?.textContent ?? "").toBe(
      variant.count,
    );
    if (variant.isChannel)
      expect(button.querySelector(".lucide-check")).toBeNull();
    button.focus();
    button.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    expect(button.getAttribute("aria-expanded")).toBe("false");
    await act(async () => button.click());
    const panel = document.getElementById(
      button.getAttribute("aria-controls")!,
    )!;
    expect(panel).not.toBeNull();
    expect(panel.tabIndex).toBe(0);
    expect(host.contains(panel)).toBe(true);
    expect(panel.parentElement).toBe(button.parentElement);
    expect(panel.textContent).toContain(variant.label);
    expect(panel.textContent).toContain(variant.detail);
    await act(async () =>
      panel.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
    );
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(button);
  }
  await act(async () => root.unmount());
  host.remove();
});

it.each([
  ["sender", "Received from the sender"],
  ["member", "Received from another member"],
  ["post_office", "Received from a post office"],
  ["linked_device", "Received from a linked device"],
  [undefined, "Reception source unknown"],
] as const)(
  "discloses incoming %s message ingress without changing author or custody",
  async (receivedVia, label) => {
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
    await i18n.changeLanguage("en");
    const host = document.createElement("div");
    const root = createRoot(host);
    try {
      await act(async () =>
        root.render(
          createElement(DeliveryFooter, {
            message: { ...base, fromMe: false, receivedVia },
            isChannel: true,
            showTime: false,
          }),
        ),
      );
      const button = host.querySelector<HTMLButtonElement>(
        '[data-testid="message-source-trigger"]',
      );
      expect(button?.getAttribute("aria-label")).toBe(label);
      await act(async () => button!.click());
      expect(
        host.querySelector('[data-testid="message-source"]')?.textContent,
      ).toContain("first reached this device");
      for (const hidden of [
        { fromMe: true },
        { recalled: true },
        { pending: true },
      ]) {
        await act(async () =>
          root.render(
            createElement(DeliveryFooter, {
              message: { ...base, fromMe: false, receivedVia, ...hidden },
              isChannel: true,
            }),
          ),
        );
        expect(
          host.querySelector('[data-testid="message-source-trigger"]'),
        ).toBeNull();
      }
    } finally {
      await act(async () => root.unmount());
    }
  },
);

it("retires open reception details on source, message, conversation and session replacement", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  await i18n.changeLanguage("en");
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const original: ChatMessage = {
    ...base,
    fromMe: false,
    receivedVia: "member",
    arrivalKind: "catch_up",
    receivedAt: 42,
  };
  const show = (message: ChatMessage) =>
    act(async () =>
      root.render(createElement(DeliveryFooter, { message, isChannel: true })),
    );
  const trigger = () =>
    host.querySelector<HTMLButtonElement>(
      '[data-testid="message-source-trigger"]',
    )!;
  try {
    await show(original);
    expect(host.querySelector('[data-testid="message-time"]')).not.toBeNull();
    expect(
      host.querySelector('[data-testid="delayed-message"]'),
    ).not.toBeNull();
    await act(async () => trigger().click());
    const panel = host.querySelector<HTMLElement>(
      '[data-testid="message-source"]',
    )!;
    expect(panel.textContent).toContain("not describe the full route");
    await act(async () =>
      panel.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
    );
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(trigger());
    for (const replacement of [
      { ...original, receivedVia: "sender" as const },
      { ...original, id: "another-message" },
    ]) {
      await act(async () => trigger().click());
      await show(replacement);
      expect(host.querySelector('[data-testid="message-source"]')).toBeNull();
      await show(original);
    }
    const savedActive = useChat.getState().active;
    const savedGeneration = useAuth.getState().generation;
    try {
      for (const invalidate of [
        () =>
          useChat.setState({
            active: { kind: "channel", id: "other", name: "Other" },
          }),
        () =>
          useAuth.setState((state) => ({ generation: state.generation + 1 })),
      ]) {
        await act(async () => trigger().click());
        await act(async () => invalidate());
        expect(host.querySelector('[data-testid="message-source"]')).toBeNull();
      }
    } finally {
      await act(async () => {
        useChat.setState({ active: savedActive });
        useAuth.setState({ generation: savedGeneration });
      });
    }
    await show({
      ...original,
      announcement: {
        revision: "r",
        content: "notice",
        author: "owner",
        authored_at: 1,
      },
    });
    expect(
      host.querySelector('[data-testid="message-source-trigger"]'),
    ).toBeNull();
  } finally {
    await act(async () => root.unmount());
    host.remove();
  }
});
