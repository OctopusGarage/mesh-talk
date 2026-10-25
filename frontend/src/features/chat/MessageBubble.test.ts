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

it.each([
  { from_me: true, who: ["offline-sibling"], mine: true },
  { from_me: false, who: ["local-device"], mine: false },
  { who: ["local-device"], mine: true },
])(
  "uses certified ownership consistently for chip styling and accessible actions: %j",
  ({ mine, ...ownership }) => {
    const reaction: ReactionInfo = {
      target: "target",
      emoji: "👍",
      ...ownership,
    };
    const html = renderToStaticMarkup(
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
        reactions: [reaction],
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
