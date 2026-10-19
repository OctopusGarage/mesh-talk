import { useMemo } from "react";

/** Highlight every case-insensitive occurrence of `term` in `text` with the signal hue. */
export function HighlightedText({
  text,
  term,
}: {
  text: string;
  term: string;
}) {
  const parts = useMemo(() => {
    const q = term.trim();
    if (!q) return [text];
    const re = new RegExp(
      `(${q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`,
      "gi",
    );
    return text.split(re);
  }, [text, term]);
  return (
    <>
      {parts.map((p, i) =>
        i % 2 === 1 ? (
          <mark
            key={i}
            className="rounded-sm bg-signal/20 px-0.5 font-medium text-signal"
          >
            {p}
          </mark>
        ) : (
          <span key={i}>{p}</span>
        ),
      )}
    </>
  );
}
