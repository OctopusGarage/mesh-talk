import "@/lib/i18n";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import type { ReactionInfo } from "@/lib/types";
import { MessageBubble } from "./MessageBubble";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/store/packs", () => ({
  usePacks: (select: (state: { packs: [] }) => unknown) =>
    select({ packs: [] }),
}));
vi.mock("./DeliveryFooter", () => ({ DeliveryFooter: () => null }));

function renderReactions(
  reactions: ReactionInfo[],
  readOnly = false,
  onMoreReactions?: () => void,
) {
  return renderToStaticMarkup(
    createElement(MessageBubble, {
      m: {
        id: "target",
        who: "author",
        text: "Message",
        wallClock: Date.now(),
        fromMe: false,
        replyTo: null,
      },
      parent: null,
      showAuthor: false,
      isChannel: true,
      readOnly,
      reactions,
      onMoreReactions,
      selfReactionId: "local-device",
      myName: "Me",
      onReply: vi.fn(),
      onReact: vi.fn(),
      onRetry: vi.fn(),
      onDelete: vi.fn(),
      onRecall: vi.fn(),
      onReEdit: vi.fn(),
    }),
  );
}

it.each([
  { from_me: true, count: 1, mine: true },
  { from_me: false, count: 1, mine: false },
  { from_me: true, count: 100000, mine: true },
])(
  "uses certified ownership consistently for chip styling and accessible actions: %j",
  ({ mine, ...ownership }) => {
    const reaction: ReactionInfo = {
      target: "target",
      emoji: "👍",
      ...ownership,
    };
    const html = renderReactions([reaction]);
    expect(html).toContain(
      mine ? "Remove your 👍 reaction" : "Add 👍 reaction",
    );
    const chip = html.match(
      /<button[^>]*data-testid="reaction-chip"[^>]*>/,
    )?.[0];
    expect(chip).toBeDefined();
    if (mine) expect(chip).toContain("border-signal");
    else expect(chip).toContain("border-border");
  },
);

it("keeps inline reactions read-only without adding a summary button", () => {
  const html = renderReactions(
    [{ target: "target", emoji: "👍", from_me: true, count: 1 }],
    true,
    vi.fn(),
  );
  const chip = html.match(/<button[^>]*data-testid="reaction-chip"[^>]*>/)?.[0];
  expect(chip).toBeDefined();
  expect(chip).toMatch(/\sdisabled(?:=|\s|>)/);
  expect(html).not.toContain('aria-label="More reactions"');
  expect(html).not.toContain("lucide-smile-plus");
});

it.each([false, true])(
  "never renders an inline summary action for unloaded or empty reactions (readOnly=%s)",
  (readOnly) => {
    const html = renderReactions([], readOnly, vi.fn());
    expect(html).not.toContain('aria-label="More reactions"');
    expect(html).not.toContain("lucide-smile-plus");
  },
);
