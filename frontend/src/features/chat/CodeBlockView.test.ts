// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";
import { CodeBlockView } from "./CodeBlockView";

it("renders code literally, copies only on request, and bounds long content", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const host = document.createElement("div");
  const root = createRoot(host);
  const copy = vi.fn();
  const code = "<script>@alice https://example.com</script>\n".repeat(30);
  try {
    await act(async () =>
      root.render(
        createElement(CodeBlockView, {
          block: { code, language: "html" },
          onCopy: copy,
        }),
      ),
    );
    expect(host.querySelector("code")!.textContent).toBe(code);
    expect(host.querySelector("script,a")).toBeNull();
    expect(host.querySelector("pre")!.className).toContain("max-h-64");
    expect(copy).not.toHaveBeenCalled();
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>('[data-testid="code-copy"]')!
        .click(),
    );
    expect(copy).toHaveBeenCalledOnce();
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>('[data-testid="code-expand"]')!
        .click(),
    );
    expect(host.querySelector("pre")!.className).toContain("max-h-[70vh]");
  } finally {
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
  }
});

it("copies the code body through the bubble privacy owner without exposing fence metadata", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const { MessageBubble } = await import("./MessageBubble");
  const { copySensitiveText } = await import("@/lib/sensitiveClipboard");
  const host = document.createElement("div");
  const root = createRoot(host);
  const code = "  @alice https://example.com\n";
  try {
    await act(async () =>
      root.render(
        createElement(MessageBubble, {
          m: {
            id: "code",
            who: "author",
            text: "```bash\n" + code + "\n```",
            wallClock: 1,
            fromMe: false,
            replyTo: null,
          },
          reactions: [],
          selfReactionId: "me",
          parent: null,
          showAuthor: false,
          isChannel: false,
          myName: "Me",
          onReply: vi.fn(),
          onReact: vi.fn(),
          onRetry: vi.fn(),
          onDelete: vi.fn(),
          onRecall: vi.fn(),
          onReEdit: vi.fn(),
        }),
      ),
    );
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>('[data-testid="code-copy"]')!
        .click(),
    );
    expect(copySensitiveText).toHaveBeenCalledWith(code, expect.any(String));
  } finally {
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
  }
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/lib/sensitiveClipboard", () => ({
  copySensitiveText: vi.fn(async () => true),
}));
vi.mock("./DeliveryFooter", () => ({ DeliveryFooter: () => null }));
