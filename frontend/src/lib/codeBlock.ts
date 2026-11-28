/** Whole-message fenced code, carried by the existing encrypted text contract. */
export type CodeBlock = { code: string; language: string };

export function decodeCodeBlock(
  text: string | null | undefined,
): CodeBlock | null {
  if (typeof text !== "string") return null;
  const firstNewline = text.indexOf("\n");
  if (firstNewline < 0) return null;
  const opening = /^(`{3,})([a-zA-Z0-9_+-]{0,32})$/.exec(
    text.slice(0, firstNewline),
  );
  if (!opening) return null;
  const fence = opening[1];
  const ending = `\n${fence}`;
  if (!text.endsWith(ending)) return null;
  const code = text.slice(firstNewline + 1, -ending.length);
  // An earlier delimiter makes this multiple blocks or mixed prose, not one card.
  if (code.split("\n").some((line) => line === fence)) return null;
  return { code, language: opening[2] };
}

export function encodeCodeBlock(code: string, language = ""): string {
  const safeLanguage = /^[a-zA-Z0-9_+-]{0,32}$/.test(language) ? language : "";
  let longest = 2;
  let run = 0;
  for (const character of code) {
    run = character === "`" ? run + 1 : 0;
    longest = Math.max(longest, run);
  }
  const fence = "`".repeat(longest + 1);
  return `${fence}${safeLanguage}\n${code}\n${fence}`;
}
