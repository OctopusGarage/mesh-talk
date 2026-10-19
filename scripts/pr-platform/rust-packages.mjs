/** One selector and package inventory for local validation and both platform guests. */
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
export const rustPackages = [
  { kind: "core", package: "mesh-talk-core", directory: "crates/mesh-talk-core", library: "mesh_talk_core", filters: "coreFilters", flag: "--core-filter", targets: "coreTests", targetFlag: "--core-test" },
  { kind: "host", package: "mesh-talk", directory: "src-tauri", library: "mesh_talk", filters: "hostFilters", flag: "--host-filter", frontendBuild: true, buildInputs: /^(?:tauri(?:\.[^/]+)?\.conf(?:\.[^/]+)?\.json|capabilities\/.*\.(?:json|toml)|windows\.manifest|icons\/icon\.(?:png|ico|icns)|Info\.plist|entitlements\.plist)$/u },
  { kind: "integration", package: "mesh-talk-integrations", directory: "crates/mesh-talk-integrations", library: "mesh_talk_integrations", filters: "integrationFilters", flag: "--integration-filter", optional: true },
  { kind: "mcp", package: "mesh-talk-mcp", directory: "crates/mesh-talk-mcp", library: "mesh_talk_mcp", filters: "mcpFilters", flag: "--mcp-filter", optional: true },
];

export const rustSelectorKeys = rustPackages.flatMap(pkg => [pkg.filters, ...(pkg.targets ? [pkg.targets] : [])]);
export const rustSelectorFlags = Object.fromEntries(rustPackages.flatMap(pkg => [[pkg.flag, pkg.filters], ...(pkg.targets ? [[pkg.targetFlag, pkg.targets]] : [])]));
export const selectedRustPackages = plan => rustPackages.filter(pkg => plan[pkg.filters]?.length || (pkg.targets && plan[pkg.targets]?.length));
export const sharedRustChange = file => /^(?:Cargo\.(?:toml|lock)$|\.cargo\/|rust-toolchain(?:\.toml)?$|\.?clippy\.toml$)/u.test(file);
export function affectsRustPackage(file, pkg) {
  if (!file.startsWith(`${pkg.directory}/`)) return false;
  const relative = file.slice(pkg.directory.length + 1);
  return sharedRustChange(relative) || /^(?:src\/|tests\/|examples\/|benches\/|build\.rs$)/u.test(relative) || !!pkg.buildInputs?.test(relative);
}
export const affectedRustPackages = files => rustPackages.filter(pkg => files.some(file => sharedRustChange(file) || affectsRustPackage(file, pkg)));
export const rustSelectionNeeded = files => affectedRustPackages(files).length > 0;

// Accept argv paths or newline-delimited git output without shell interpolation.
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv[2] !== "--selection-needed") throw Error("Usage: rust-packages.mjs --selection-needed [FILE ...]");
  const files = process.argv.length > 3 ? process.argv.slice(3) : readFileSync(0, "utf8").split(/\r?\n/u).filter(Boolean);
  process.exitCode = rustSelectionNeeded(files) ? 0 : 1;
}
