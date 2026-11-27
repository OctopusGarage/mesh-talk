import { expect, it } from "vitest";
import { decodeCodeBlock, encodeCodeBlock } from "./codeBlock";

it("round trips indentation, blank lines, CRLF, and embedded fences", () => {
  for (const code of [
    "  first\n\tsecond\n\n",
    "\r\n  x\r\n",
    "```js\n```\n````",
    "",
    "<script>@alice https://example.com</script>",
  ]) {
    expect(decodeCodeBlock(encodeCodeBlock(code, "typescript"))).toEqual({
      code,
      language: "typescript",
    });
  }
});
it("accepts an entire standard fenced message only", () => {
  expect(decodeCodeBlock("```rust\nfn main() {}\n```")).toEqual({
    code: "fn main() {}",
    language: "rust",
  });
  for (const text of [
    "ordinary",
    "before\n```\nx\n```",
    "```\nx",
    "```\nx\n```\nafter",
    "```html onclick=x\nx\n```",
    "```\nx\n```\ny\n```",
    "````\nx\n```",
  ]) {
    expect(decodeCodeBlock(text)).toBeNull();
  }
});

it("treats absent legacy text as non-code", () => {
  // History/IPC projections can omit text even when the presentation type requires it.
  expect(decodeCodeBlock(undefined)).toBeNull();
  expect(decodeCodeBlock(null)).toBeNull();
});
