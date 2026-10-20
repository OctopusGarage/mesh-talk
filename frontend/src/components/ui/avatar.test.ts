import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import { Avatar } from "./avatar";

it("renders the provided photo with its accessible name and requested dimensions", () => {
  const html = renderToStaticMarkup(
    createElement(Avatar, {
      id: "alice",
      name: "Alice",
      src: "data:image/png;base64,AAAA",
      size: 48,
    }),
  );
  expect(html).toContain('alt="Alice"');
  expect(html).toContain('src="data:image/png;base64,AAAA"');
  expect(html).toContain('width="48"');
  expect(html).toContain('height="48"');
});

it("falls back to a stable identity glyph and uses the id when the name is empty", () => {
  const render = () =>
    renderToStaticMarkup(createElement(Avatar, { id: "alice", name: "" }));
  const html = render();
  expect(html).toContain("<svg");
  expect(html).toContain("alice");
  expect(html).not.toContain("<img");
  expect(render()).toBe(html);
});
