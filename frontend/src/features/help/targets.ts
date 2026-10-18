import type { HelpTarget } from "../../../../tools/help/search.mjs";
import { helpTopics } from "./content";

/** Callers provide a topic ID, never an executable URL, action or CSS selector. */
export function resolveHelpTarget(topicId: string): HelpTarget | null {
  return helpTopics.find((topic) => topic.id === topicId)?.target ?? null;
}
