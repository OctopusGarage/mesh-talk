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

const relay = {
  user_id: "relay-device",
  account_id: "relay-account",
  name: "Post office",
  ip: "127.0.0.1",
  tcp_port: 47475,
  post_office: true,
  last_seen_secs: 0,
};

it("uses stable postal emblems for infrastructure, while people retain identity glyphs", () => {
  const render = (account_id: string, post_office = true) =>
    renderToStaticMarkup(
      createElement(PeerRow, { p: { ...relay, account_id, post_office } }),
    );
  const variants = ["a", "b", "c", "d"].map((id) => {
    const html = render(id);
    expect(html).toBe(render(id));
    expect(html).toContain('width="28"');
    return html.match(/data-post-office-avatar="([^"]+)"/)?.[1];
  });
  expect(variants).not.toContain(undefined);
  expect(new Set(variants).size).toBe(4);
  expect(render("a", false)).not.toContain("data-post-office-avatar");
});

it("preserves a post office's custom avatar", () => {
  const html = renderToStaticMarkup(
    createElement(PeerRow, {
      p: { ...relay, account_id: "peer-account" },
    }),
  );
  expect(html).toContain('src="data:image/png;base64,AAAA"');
  expect(html).not.toContain("data-post-office-avatar");
});
