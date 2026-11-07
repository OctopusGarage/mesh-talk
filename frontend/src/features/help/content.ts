import catalog from "../../../../docs/help/catalog.json";
import { HELP_VERSION } from "./metadata";
export { HELP_VERSION } from "./metadata";
import type { Topic } from "../../../../tools/help/search.mjs";

const documents = import.meta.glob<string>(
  "../../../../docs/help/topics/*.md",
  { query: "?raw", import: "default", eager: true },
);
export const HELP_LANGUAGE = catalog.language;
export const HELP_URL = `https://octopusgarage.github.io/mesh-talk/help/v/${HELP_VERSION}/`;
export const helpTopics: Topic[] = catalog.topics.map((topic) => ({
  ...topic,
  platforms: topic.platforms as Topic["platforms"],
  target: topic.target as Topic["target"],
  body: documents[`../../../../docs/help/${topic.document}`],
}));
