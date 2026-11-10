/** Device-local bindings. The host validates these again before registering with the OS. */
export interface ScreenshotShortcuts {
  capture: string | null;
  capture_hidden: string | null;
  global: boolean;
}

export const DEFAULT_SCREENSHOT_SHORTCUTS: Readonly<ScreenshotShortcuts> = {
  capture: "CommandOrControl+Shift+KeyX",
  capture_hidden: "CommandOrControl+Alt+Shift+KeyX",
  global: false,
};

export const shortcutPlatformIsMac = () =>
  typeof navigator !== "undefined" &&
  /Macintosh|Mac OS X/i.test(navigator.userAgent);

const MODIFIERS = ["CommandOrControl", "Control", "Alt", "Shift", "Super"];
const KEY = /^(Key[A-Z]|Digit[0-9]|F(?:[1-9]|1[0-2]))$/;

function parts(binding: string) {
  const items = binding.split("+");
  const code = items.pop()!;
  if (!KEY.test(code) || items.length === 0) return null;
  if (
    items.some(
      (mod, index) =>
        !MODIFIERS.includes(mod) ||
        (index > 0 &&
          MODIFIERS.indexOf(items[index - 1]) >= MODIFIERS.indexOf(mod)),
    )
  )
    return null;
  if (
    items.includes("CommandOrControl") &&
    (items.includes("Control") || items.includes("Super"))
  )
    return null;
  if (!items.some((mod) => mod !== "Shift")) return null;
  return { mods: items, code };
}

function resolved(binding: string, mac: boolean) {
  const parsed = parts(binding);
  if (!parsed) return null;
  const has = (mod: string) => parsed.mods.includes(mod);
  return {
    code: parsed.code,
    ctrl: has("Control") || (!mac && has("CommandOrControl")),
    meta: has("Super") || (mac && has("CommandOrControl")),
    alt: has("Alt"),
    shift: has("Shift"),
  };
}

export function bindingProblem(
  binding: string | null,
): "invalid" | "reserved" | null {
  if (binding === null) return null;
  const parsed = parts(binding);
  if (!parsed) return "invalid";
  // Protect familiar editing, navigation, app and window commands on both platforms.
  const { mods, code } = parsed;
  if (mods.includes("Alt") && code === "F4") return "reserved";
  if (mods.includes("Super") && !mods.includes("CommandOrControl"))
    return "reserved";
  if (
    (mods.includes("CommandOrControl") || mods.includes("Control")) &&
    !mods.includes("Alt")
  ) {
    if (!mods.includes("Shift") && /^Key[ACFHLNOPQRSTVWXZ]$/.test(code))
      return "reserved";
    if (mods.includes("Shift") && /^Key[NPQTVWZ]$/.test(code))
      return "reserved";
  }
  return null;
}

export function screenshotShortcutProblem(
  value: ScreenshotShortcuts,
  mac = shortcutPlatformIsMac(),
) {
  const problem =
    bindingProblem(value.capture) ?? bindingProblem(value.capture_hidden);
  if (problem) return problem;
  if (value.global && !value.capture && !value.capture_hidden) return "empty";
  if (
    value.capture &&
    value.capture_hidden &&
    JSON.stringify(resolved(value.capture, mac)) ===
      JSON.stringify(resolved(value.capture_hidden, mac))
  )
    return "duplicate";
  return null;
}

/** Store physical letter/digit keys, avoiding shifted glyphs and IME text. */
export function bindingFromKeyboard(
  event: Pick<
    KeyboardEvent,
    | "code"
    | "ctrlKey"
    | "metaKey"
    | "altKey"
    | "shiftKey"
    | "repeat"
    | "isComposing"
  >,
  mac = shortcutPlatformIsMac(),
): string | null {
  if (event.repeat || event.isComposing || !KEY.test(event.code)) return null;
  const primary = mac ? event.metaKey : event.ctrlKey;
  if (primary && (mac ? event.ctrlKey : event.metaKey)) return null;
  const mods = [
    primary && "CommandOrControl",
    mac && !primary && event.ctrlKey && "Control",
    event.altKey && "Alt",
    event.shiftKey && "Shift",
    !mac && event.metaKey && "Super",
  ].filter(Boolean);
  const binding = [...mods, event.code].join("+");
  return parts(binding) ? binding : null;
}

export function matchesShortcut(
  event: KeyboardEvent,
  binding: string | null,
  mac = shortcutPlatformIsMac(),
) {
  if (!binding || bindingProblem(binding) || event.repeat || event.isComposing)
    return false;
  const wanted = resolved(binding, mac);
  return (
    !!wanted &&
    event.code === wanted.code &&
    event.ctrlKey === wanted.ctrl &&
    event.metaKey === wanted.meta &&
    event.altKey === wanted.alt &&
    event.shiftKey === wanted.shift
  );
}

export function formatShortcut(
  binding: string | null,
  mac = shortcutPlatformIsMac(),
): string {
  if (!binding) return "";
  const value = parts(binding);
  if (!value) return "";
  const names: Record<string, string> = mac
    ? { CommandOrControl: "⌘", Control: "⌃", Alt: "⌥", Shift: "⇧", Super: "⌘" }
    : {
        CommandOrControl: "Ctrl",
        Control: "Ctrl",
        Alt: "Alt",
        Shift: "Shift",
        Super: "Win",
      };
  return [
    ...value.mods.map((mod) => names[mod]),
    value.code.replace(/^(Key|Digit)/, ""),
  ].join(mac ? "" : "+");
}

export function ariaShortcut(
  binding: string | null,
  mac = shortcutPlatformIsMac(),
): string | undefined {
  if (!binding) return undefined;
  const value = resolved(binding, mac);
  if (!value) return undefined;
  return [
    value.ctrl && "Control",
    value.alt && "Alt",
    value.shift && "Shift",
    value.meta && "Meta",
    value.code.replace(/^(Key|Digit)/, ""),
  ]
    .filter(Boolean)
    .join("+");
}

export const EDITOR_TOOL_KEYS = {
  select: "V",
  rect: "R",
  ellipse: "O",
  arrow: "A",
  pen: "P",
  mosaic: "M",
  text: "T",
} as const;
export function editorToolForKey(
  event: KeyboardEvent,
): keyof typeof EDITOR_TOOL_KEYS | null {
  if (
    event.repeat ||
    event.isComposing ||
    event.ctrlKey ||
    event.metaKey ||
    event.altKey ||
    event.shiftKey
  )
    return null;
  return (
    (Object.keys(EDITOR_TOOL_KEYS) as (keyof typeof EDITOR_TOOL_KEYS)[]).find(
      (tool) => event.code === `Key${EDITOR_TOOL_KEYS[tool]}`,
    ) ?? null
  );
}
