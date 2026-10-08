import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

const { avatars } = vi.hoisted(() => ({
  avatars: { "peer-account": "data:image/png;base64,AAAA" } as Record<
    string,
    string
  >,
}));

vi.mock("@/store/avatars", () => ({
  useAvatar: (id: string) => avatars[id],
}));

import { PeerRow } from "./DiagnosticsDialog";

it("shows a discovered peer's account avatar in Diagnostics", () => {
  const html = renderToStaticMarkup(
    createElement(PeerRow, {
      p: {
        user_id: "peer-device",
        account_id: "peer-account",
        name: "Alice",
        ip: "127.0.0.1",
        tcp_port: 47474,
        post_office: false,
        last_seen_secs: 0,
      },
    }),
  );

  expect(html).toContain("Alice");
  expect(html).toContain('src="data:image/png;base64,AAAA"');
});
