import catalog from "virtual:mesh-asset-catalog";

/** Preset filenames are discovered once by Vite; images stay ordinary static
 *  files, so importing chat does not request a JS module for every image. */
export function assetEntries(dir: string): Array<[string, string]> {
  return (catalog[dir] ?? []).map((name) => [
    `${dir}/${name}`,
    `/${dir}/${encodeURIComponent(name)}`,
  ]);
}
