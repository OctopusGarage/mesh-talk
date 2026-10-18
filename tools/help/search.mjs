/** Shared, deterministic search. Every query token must match; no network or Node APIs. */
export function searchHelp(topics, query, { platform, limit = 50 } = {}) {
  const tokens = String(query)
    .normalize("NFKC")
    .toLowerCase()
    .trim()
    .split(/\s+/u)
    .filter(Boolean);
  return topics
    .filter((t) => !platform || t.platforms.includes(platform))
    .map((topic, index) => {
      const fields = [
        [topic.title, 10],
        [topic.keywords.join(" "), 8],
        [topic.summary, 5],
        [topic.body, 1],
      ];
      const scores = tokens.map((token) =>
        fields.reduce(
          (n, [value, weight]) =>
            n +
            (value.normalize("NFKC").toLowerCase().includes(token)
              ? weight
              : 0),
          0,
        ),
      );
      return {
        topic,
        index,
        score: scores.every(Boolean) ? scores.reduce((a, b) => a + b, 0) : -1,
      };
    })
    .filter((x) => x.score >= 0)
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .slice(0, Math.max(0, limit))
    .map((x) => x.topic);
}
