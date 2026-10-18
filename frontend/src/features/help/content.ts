import catalog from "../../../../docs/help/catalog.json";
import workspace from "../../../../Cargo.toml?raw";
import type { Topic } from "../../../../tools/help/search.mjs";

const documents = import.meta.glob<string>(
  "../../../../docs/help/topics/*.md",
  { query: "?raw", import: "default", eager: true },
);
export const HELP_VERSION = workspace.match(
  /\[workspace\.package\][\s\S]*?\bversion\s*=\s*"([^"]+)"/,
)![1];
export const HELP_LANGUAGE = catalog.language;
export const HELP_URL = `https://octopusgarage.github.io/mesh-talk/help/v/${HELP_VERSION}/`;
export const helpTopics: Topic[] = catalog.topics.map((topic) => ({
  ...topic,
  platforms: topic.platforms as Topic["platforms"],
  target: topic.target as Topic["target"],
  body: documents[`../../../../docs/help/${topic.document}`],
}));
