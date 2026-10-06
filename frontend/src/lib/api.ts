// Stable public IPC facade. Feature modules own command names and payloads.
// Tauri v2 maps camelCase JS arg keys to the Rust commands' snake_case params.
export { privacy } from "./api/privacy";
export { auth } from "./api/auth";
export { chat } from "./api/chat";
export { calls } from "./api/calls";
export { diag, presence, obs } from "./api/diagnostics";
export { contactPolicy, favorites, avatars } from "./api/contacts";
export { settings } from "./api/settings";
