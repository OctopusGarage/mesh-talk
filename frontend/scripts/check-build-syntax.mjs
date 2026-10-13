import { parse } from "acorn";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

// Safari 15 is the build target for macOS 12's WKWebView. ES2021 parsing
// rejects class static blocks, which made the v0.1.4 sign-in window blank.
export function checkBuildSyntax(directory) {
  let checked = 0;
  function visit(path) {
    for (const entry of readdirSync(path, { withFileTypes: true })) {
      const file = join(path, entry.name);
      if (entry.isDirectory()) visit(file);
      else if (entry.isFile() && entry.name.endsWith(".js")) {
        try {
          parse(readFileSync(file, "utf8"), {
            ecmaVersion: 2021,
            sourceType: "module",
          });
        } catch (error) {
          throw new Error(`${file}: ${error.message}`, { cause: error });
        }
        checked++;
      }
    }
  }
  visit(directory);
  if (!checked) throw new Error(`No JavaScript files found in ${directory}`);
  return checked;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const directory =
    process.argv[2] ?? fileURLToPath(new URL("../dist", import.meta.url));
  console.log(
    `Checked ${checkBuildSyntax(directory)} JavaScript files for macOS 12 compatibility`,
  );
}
