// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { HighlightedText } from "./HighlightedText";

let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  host = document.createElement("div");
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
async function render(text: string, term: string) {
  await act(async () =>
    root.render(createElement(HighlightedText, { text, term })),
  );
  expect(host.textContent).toBe(text);
  return [...host.querySelectorAll("mark")].map((mark) => mark.textContent);
}
it.each([".*+?^${}()|[]\\", "[a]", "a.b", "(a)", "$5"])(
  "matches regex syntax %s literally",
  async (term) => {
    expect(await render(`before ${term} after`, term)).toEqual([term]);
  },
);
it("marks every case-insensitive match and preserves surrounding text", async () => {
  expect(await render("prefix OneONE one! suffix", "  one  ")).toEqual([
    "One",
    "ONE",
    "one",
  ]);
  expect(host.querySelector("mark")?.className).toBe(
    "rounded-sm bg-signal/20 px-0.5 font-medium text-signal",
  );
});
it.each(["", " \t\n "])("does not mark an empty query %j", async (term) => {
  expect(await render("Untouched  content\n中文🙂", term)).toEqual([]);
});
it("preserves text without any matches", async () => {
  expect(await render("unmatched text", "absent")).toEqual([]);
});
it("renders markup as text and supports empty text", async () => {
  expect(await render("<script>hello</script>", "script")).toEqual([
    "script",
    "script",
  ]);
  expect(host.querySelector("script")).toBeNull();
  expect(await render("", "word")).toEqual([]);
});
it("updates highlights when text and query change", async () => {
  await render("one two", "one");
  expect(await render("TWO three", "two")).toEqual(["TWO"]);
});
