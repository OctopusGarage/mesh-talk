// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { DeliveryFooter } from "./DeliveryFooter";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import type { ChatMessage } from "@/store/conversationModel";

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
