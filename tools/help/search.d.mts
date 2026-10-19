export type HelpTarget =
  | {
      kind: "settings";
      category:
        | "general"
        | "appearance"
        | "notifications"
        | "connection"
        | "privacy"
        | "files"
        | "history";
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
