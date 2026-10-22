import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import i18n, { SUPPORTED_LANGUAGES } from "@/lib/i18n";
import { LoginScreen } from "./LoginScreen";

const { authState } = vi.hoisted(() => ({
  authState: {
    login: vi.fn(),
    register: vi.fn(),
    clearError: vi.fn(),
    loading: false,
    error: null as string | null,
  },
}));

vi.mock("@/store/auth", () => ({ useAuth: () => authState }));
vi.mock("@/lib/api", () => ({ settings: { get: vi.fn() } }));
vi.mock("./RestoreBackup", () => ({ RestoreBackup: () => null }));
vi.mock("@/features/help/HelpDialog", () => ({ HelpDialog: () => null }));

function alertText() {
  const html = renderToStaticMarkup(createElement(LoginScreen));
  return html.match(/<p role="alert"[^>]*>(.*?)<\/p>/)?.[1] ?? null;
}

afterEach(async () => {
  authState.error = null;
  await i18n.changeLanguage("en");
});

it.each([
  ["en", "Invalid username or password"],
  ["es", "Nombre de usuario o contraseña incorrectos"],
  ["ja", "ユーザー名またはパスワードが正しくありません"],
  ["zh-Hans", "用户名或密码错误"],
  ["zh-Hant", "使用者名稱或密碼錯誤"],
  ["yue", "用戶名或密碼錯誤"],
])("renders the generic credential error in %s", async (language, expected) => {
  authState.error = "Invalid username or password";
  await i18n.changeLanguage(language);

  expect(alertText()).toBe(expected);
});

it("uses the current locale when presenting an existing credential failure", async () => {
  authState.error = "Invalid username or password";
  await i18n.changeLanguage("en");
  expect(alertText()).toBe("Invalid username or password");

  await i18n.changeLanguage("zh-Hans");
  expect(alertText()).toBe("用户名或密码错误");
});

it.each(SUPPORTED_LANGUAGES)(
  "preserves unknown error details in %s",
  async (language) => {
    authState.error = "Unable to open encrypted identity store";
    await i18n.changeLanguage(language);

    expect(alertText()).toBe(authState.error);
  },
);

it("renders no error alert before a failure", () => {
  expect(alertText()).toBeNull();
});

it.each(SUPPORTED_LANGUAGES)(
  "never makes an empty password look prefilled in %s",
  async (language) => {
    await i18n.changeLanguage(language);
    const html = renderToStaticMarkup(createElement(LoginScreen));
    expect(html).not.toContain('placeholder="••••••••"');
    expect(html).toContain('autoComplete="current-password"');
  },
);
