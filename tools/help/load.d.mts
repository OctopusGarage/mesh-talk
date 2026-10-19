import type { Topic } from "./search.mjs";
export interface HelpData {
  schemaVersion: 1;
  language: "en";
  version: string;
  topics: Topic[];
}
export const repositoryRoot: string;
export function loadHelp(options?: { root?: string }): Promise<HelpData>;
