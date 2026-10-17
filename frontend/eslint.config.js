import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "dist",
      "coverage",
      "playwright-report",
      "test-results",
      "e2e/__screens__",
      "**/*.config.{js,ts}",
    ],
  },
  {
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2020,
      globals: globals.browser,
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      "react-hooks/rules-of-hooks": "error",
      // The whole architecture relies on correct effect deps + cleanup; a missing
      // cleanup leaks listeners/timers. Enforce as an error so CI guards it.
      "react-hooks/exhaustive-deps": "error",
      "react-refresh/only-export-components": [
        "warn",
        { allowConstantExport: true },
      ],
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
    },
  },
  {
    // IPC adapters stay independent of app stores and feature components.
    files: ["src/lib/api/**/*.{ts,tsx}"],
    rules: {
      "no-restricted-imports": [
        "error",
        {
          patterns: [
            "@/store/**",
            "@/features/**",
            "**/store/**",
            "**/features/**",
          ],
        },
      ],
    },
  },
  {
    // The public API facade composes the auth owner for settings. Importing that
    // facade from auth would recreate an eager runtime cycle.
    files: ["src/store/auth.ts"],
    rules: {
      "no-restricted-imports": [
        "error",
        {
          paths: [
            {
              name: "@/lib/api",
              message: "Import the auth/chat IPC adapters directly.",
            },
          ],
        },
      ],
    },
  },
  {
    // shadcn/ui primitives intentionally co-export variant helpers with components.
    files: ["src/components/ui/**/*.{ts,tsx}"],
    rules: { "react-refresh/only-export-components": "off" },
  },
);
