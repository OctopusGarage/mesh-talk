// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { useAuth } from "@/store/auth";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";

vi.mock("@/store/chat", () => ({
  useChat: (selector: (state: unknown) => unknown) =>
    selector({ peers: [{ account_id: "alice", user_id: "device-alice" }] }),
}));
vi.mock("@/store/presence", () => ({
  usePresenceFor: () => null,
  presenceStatus: () => "offline",
}));
vi.mock("@/lib/motion", () => ({ useMotionOK: () => false }));

import { VerifyContactDialog } from "./VerifyContactDialog";

it("labels an unverified contact's safety action without claiming trust", async () => {
  await i18n.changeLanguage("en");
  const html = renderToStaticMarkup(
    createElement(VerifyContactDialog, { accountId: "alice", name: "Alice" }),
  );

  expect(html).toContain('data-trust="unverified"');
  expect(html).toContain(`title="${i18n.t("verify.title")}"`);
  expect(html).not.toContain('data-trust="verified"');
});

const api = vi.hoisted(() => ({ getTrust: vi.fn(), markVerified: vi.fn() }));
vi.mock("@/lib/api", () => ({ chat: api }));
it("ignores a verification history response after the local session changes", async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  let finish!: (value: unknown) => void;
  api.getTrust
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockImplementation(() => new Promise(() => {}));
  useAuth.setState({
    user: { id: "owner", username: "owner", display_name: "Owner" },
    generation: 1,
  });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  try {
    await act(async () =>
      root.render(
        createElement(VerifyContactDialog, {
          accountId: "alice",
          name: "Alice",
        }),
      ),
    );
    await act(async () => {
      useAuth.setState({ generation: 2 });
      finish({ verified: true, fingerprint_changed: false });
    });
    expect(
      host
        .querySelector('[data-testid="verify-trigger"]')
        ?.getAttribute("data-trust"),
    ).toBe("unverified");
  } finally {
    await act(async () => root.unmount());
    host.remove();
  }
});
