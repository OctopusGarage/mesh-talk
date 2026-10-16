import { ESLint } from "eslint";
import { fileURLToPath } from "node:url";
import { expect, it } from "vitest";

const eslint = new ESLint({
  cwd: fileURLToPath(new URL("../..", import.meta.url)),
});

it.each([
  "@/store/chat",
  "@/features/chat/Sidebar",
  "../../store/chat",
  "../../../store/auth",
  "../../../features/chat/Sidebar",
])("keeps IPC adapters independent of state and UI via %s", async (source) => {
  const [result] = await eslint.lintText(
    `import value from "${source}";\nvoid value;`,
    {
      filePath: "src/lib/api/nested/adapter.ts",
    },
  );
  expect(
    result.messages.some(
      (message) => message.ruleId === "no-restricted-imports",
    ),
  ).toBe(true);
});

it("allows adapters to depend on shared types and sibling adapters", async () => {
  const [result] = await eslint.lintText(
    'import type { PeerInfo } from "../types";\nexport type Peer = PeerInfo;',
    {
      filePath: "src/lib/api/adapter.ts",
    },
  );
  expect(
    result.messages.filter(
      (message) => message.ruleId === "no-restricted-imports",
    ),
  ).toEqual([]);
});
