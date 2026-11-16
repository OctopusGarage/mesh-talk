import workspace from "../../../../Cargo.toml?raw";

export const HELP_VERSION = workspace.match(
  /\[workspace\.package\][\s\S]*?\bversion\s*=\s*"([^"]+)"/,
)![1];
