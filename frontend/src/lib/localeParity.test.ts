import { expect, it } from "vitest";
import en from "@/locales/en.json";
import { SUPPORTED_LANGUAGES, LANGUAGE_LABELS } from "./i18n";

const locales = import.meta.glob<Record<string, unknown>>("../locales/*.json", {
  eager: true,
  import: "default",
});

function keys(value: Record<string, unknown>, prefix = ""): string[] {
  return Object.entries(value).flatMap(([name, entry]) => {
    const path = prefix ? `${prefix}.${name}` : name;
    return entry && typeof entry === "object" && !Array.isArray(entry)
      ? keys(entry as Record<string, unknown>, path)
      : [path];
  });
}

function values(
  value: Record<string, unknown>,
  prefix = "",
): Record<string, string> {
  return Object.fromEntries(
    Object.entries(value).flatMap(([name, entry]) => {
      const path = prefix ? `${prefix}.${name}` : name;
      return entry && typeof entry === "object" && !Array.isArray(entry)
        ? Object.entries(values(entry as Record<string, unknown>, path))
        : [[path, String(entry)]];
    }),
  );
}

function variables(value: string) {
  return [...value.matchAll(/{{\s*([\w-]+)\s*}}/g)]
    .map((match) => match[1])
    .sort();
}

it("keeps all supported locales aligned with the English interface", () => {
  const registered = [...SUPPORTED_LANGUAGES].sort();
  expect(
    Object.keys(locales)
      .map((path) => path.split("/").pop()!.replace(".json", ""))
      .sort(),
  ).toEqual(registered);
  expect(Object.keys(LANGUAGE_LABELS)).toEqual([...SUPPORTED_LANGUAGES]);
  const reference = keys(en).sort();
  for (const [path, locale] of Object.entries(locales)) {
    expect(keys(locale).sort(), path).toEqual(reference);
    const translated = values(locale);
    for (const [key, source] of Object.entries(values(en))) {
      expect(translated[key]?.trim(), `${path}: ${key}`).toBeTruthy();
      expect(variables(translated[key]), `${path}: ${key}`).toEqual(
        variables(source),
      );
    }
  }
});
