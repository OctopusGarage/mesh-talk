/** Conservative source-import reachability, including type imports and literal test mocks. */
import path from "node:path";
import { createRequire } from "node:module";
const require = createRequire(
  new URL("../../frontend/package.json", import.meta.url),
);
const sourceExtension = /\.[cm]?[jt]sx?$/u;
export const isFrontendTest = (file) =>
  /^frontend\/src\/.*\.test\.ts$/u.test(file);

export function buildFrontendGraph(current, previous = new Map()) {
  const ts = require("typescript");
  const available = new Set([...previous.keys(), ...current.keys()]);
  const reverse = new Map();
  const computed = new Set();
  function dependency(file, specifier) {
    if (!specifier.startsWith(".") && !specifier.startsWith("@/")) return;
    const base = specifier.startsWith("@/")
      ? `frontend/src/${specifier.slice(2)}`
      : path.posix.normalize(
          path.posix.join(path.posix.dirname(file), specifier),
        );
    const candidates = [
      base,
      ...[".ts", ".tsx", ".js", ".jsx", ".json"].map((ext) => base + ext),
      ...[".ts", ".tsx", ".js", ".jsx"].map((ext) => `${base}/index${ext}`),
    ];
    // Bundler resolution permits a .js specifier pointing at a TypeScript source.
    if (base.endsWith(".js"))
      candidates.push(base.slice(0, -3) + ".ts", base.slice(0, -3) + ".tsx");
    const resolved = candidates.find((candidate) => available.has(candidate));
    if (resolved) {
      if (!reverse.has(resolved)) reverse.set(resolved, new Set());
      reverse.get(resolved).add(file);
    }
  }
  for (const sources of [previous, current])
    for (const [file, contents] of sources) {
      if (!sourceExtension.test(file)) continue;
      const source = ts.createSourceFile(
        file,
        contents,
        ts.ScriptTarget.Latest,
        true,
      );
      function visit(node) {
        if (
          (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
          node.moduleSpecifier &&
          ts.isStringLiteralLike(node.moduleSpecifier)
        )
          dependency(file, node.moduleSpecifier.text);
        if (ts.isCallExpression(node)) {
          const name = node.expression.getText(source);
          if (
            [
              "import",
              "require",
              "vi.mock",
              "vi.doMock",
              "vi.importActual",
              "vi.importMock",
            ].includes(name)
          ) {
            const argument = node.arguments[0];
            if (argument && ts.isStringLiteralLike(argument))
              dependency(file, argument.text);
            else computed.add(file);
          }
          if (name === "import.meta.glob") computed.add(file);
        }
        ts.forEachChild(node, visit);
      }
      visit(source);
    }
  return { current: new Set(current.keys()), reverse, computed };
}

export function affectedFrontendTests(file, graph) {
  const visited = new Set([file]);
  const queue = [file];
  for (let i = 0; i < queue.length; i++)
    for (const importer of graph.reverse.get(queue[i]) ?? []) {
      if (!visited.has(importer)) {
        visited.add(importer);
        queue.push(importer);
      }
    }
  return {
    tests: [...visited]
      .filter((item) => isFrontendTest(item) && graph.current.has(item))
      .sort(),
    computed: [...visited].filter((item) => graph.computed.has(item)),
  };
}
