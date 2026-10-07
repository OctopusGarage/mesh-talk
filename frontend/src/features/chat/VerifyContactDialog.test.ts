import { createElement } from "react";
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
