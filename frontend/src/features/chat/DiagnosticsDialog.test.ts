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

it("labels a post-office proxy address as a gateway route", () => {
  const html = renderToStaticMarkup(
    createElement(PeerRow, {
      p: {
        user_id: "relay-device",
        account_id: "relay-account",
        name: "mac2015-post-office",
        ip: "192.168.3.16",
        tcp_port: 47476,
        post_office: true,
        via_gateway: true,
        last_seen_secs: 0,
      },
    }),
  );
  expect(html).toContain("via gateway");
  expect(html).toContain("192.168.3.16:47476");
});
