import type { SettingsCategory } from "../../frontend/src/features/chat/settings/model";

export type HelpTarget =
  | {
      kind: "settings";
      category: SettingsCategory;
      controlId?: string;
    }
  | { kind: "diagnostics" };
export interface Topic {
  id: string;
  title: string;
  summary: string;
  category: string;
  keywords: string[];
  platforms: ("macos" | "windows" | "linux")[];
  document: string;
  target?: HelpTarget;
  body: string;
}
export function searchHelp(
  topics: readonly Topic[],
  query: string,
  options?: { platform?: string; limit?: number },
): Topic[];
