import type { HelpTarget } from "../../../../tools/help/search.mjs";
import catalog from "../../../../docs/help/catalog.json";

/** Callers provide a topic ID, never an executable URL, action or CSS selector. */
export function resolveHelpTarget(topicId: string): HelpTarget | null {
  return (
    (catalog.topics.find((topic) => topic.id === topicId)?.target as
      HelpTarget | undefined) ?? null
  );
}
