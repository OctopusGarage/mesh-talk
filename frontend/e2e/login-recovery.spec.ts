import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("login settings wait for the native session to finish signing out", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const scope = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args?: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __logoutPending: boolean;
      __finishLogout: () => void;
      __ownerlessDuringLogout: number;
    };
    const invoke = scope.__TAURI_INTERNALS__.invoke.bind(
      scope.__TAURI_INTERNALS__,
    );
    scope.__logoutPending = false;
    scope.__ownerlessDuringLogout = 0;
    scope.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
      if (cmd === "logout") {
        scope.__logoutPending = true;
        await new Promise<void>((resolve) => {
          scope.__finishLogout = resolve;
        });
        scope.__logoutPending = false;
      }
      if (cmd === "get_app_settings" && scope.__logoutPending && !args.owner) {
        scope.__ownerlessDuringLogout++;
        throw new Error("settings owner required");
      }
      return invoke(cmd, args);
    };
  });
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-sign-out").click();
  await expect(page.getByTestId("chat-shell")).toBeHidden();
  await expect(page.getByTestId("login-form")).toBeHidden();
  await page.evaluate(() =>
    (window as unknown as { __finishLogout: () => void }).__finishLogout(),
  );
  await expect(page.getByTestId("login-form")).toBeVisible();
  await expect(page.getByTestId("login-stay-signed-in")).toBeEnabled();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __ownerlessDuringLogout: number })
          .__ownerlessDuringLogout,
    ),
  ).toBe(0);
});

test("a failed stay-signed-in change keeps its saved value", async ({
  page,
}) => {
  await page.goto("/");
  const stay = page.getByTestId("login-stay-signed-in");
  await expect(stay).toBeEnabled();
  await expect(stay).toBeChecked();
  await page.evaluate(() => {
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("set_app_settings");
  });
  await stay.click();
  await expect(page.getByRole("alert")).toContainText(
    "Couldn’t save the change",
  );
  await expect(stay).toBeChecked();

  await page.getByRole("button", { name: "Dismiss" }).click();
  await expect(stay).toBeEnabled();
  await stay.uncheck();
  await expect(stay).not.toBeChecked();
});

test("an empty password is clear and its visibility can be toggled", async ({
  page,
}) => {
  await page.goto("/");
  const password = page.getByTestId("login-password");
  await expect(password).toHaveValue("");
  await expect(password).toHaveAttribute(
    "placeholder",
    "Enter your account password",
  );
  await password.fill("a visible password");
  await page
    .getByRole("button", { name: "Show password", exact: true })
    .click();
  await expect(password).toHaveAttribute("type", "text");
  await page
    .getByRole("button", { name: "Hide password", exact: true })
    .click();
  await expect(password).toHaveAttribute("type", "password");
  await page.getByTestId("login-tab-register").click();
  await expect(password).toHaveValue("");
  await expect(page.getByTestId("login-stay-signed-in")).toBeVisible();
});

test("created accounts retry entry without registering or typing their password twice", async ({
  page,
}) => {
  await page.goto("/");
  await page.evaluate(() => {
    const scope = window as unknown as {
      __mockFailNext: (cmd: string) => void;
      __registrations: number;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args?: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    const invoke = scope.__TAURI_INTERNALS__.invoke.bind(
      scope.__TAURI_INTERNALS__,
    );
    scope.__registrations = 0;
    scope.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      if (cmd === "register") scope.__registrations++;
      return invoke(cmd, args);
    };
    scope.__mockFailNext("login");
  });
  await page.getByTestId("login-tab-register").click();
  await page.getByTestId("login-username").fill("new-account");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await expect(page.getByRole("status")).toContainText(
    "Your account was created",
  );
  await expect(page.getByTestId("login-password")).toHaveValue("password123");
  await page.getByTestId("login-submit").click();
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await expect(page.getByTestId("account-protection")).toContainText(
    "saved securely",
  );
  expect(
    await page.evaluate(
      () => (window as unknown as { __registrations: number }).__registrations,
    ),
  ).toBe(1);
  await page.getByRole("button", { name: "I’ll do this later" }).click();
  await expect(page.getByTestId("account-protection")).toBeHidden();
});

test("failed credential saving does not block entry and can be retried", async ({
  page,
}) => {
  await page.goto("/");
  await page.evaluate(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args?: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const invoke = internals.invoke.bind(internals);
    internals.invoke = async (cmd, args) => {
      const result = await invoke(cmd, args);
      if (cmd === "login")
        return { ...(result as object), saved_session: "failed" };
      return result;
    };
  });
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await expect(page.getByTestId("account-protection")).toContainText(
    "Your login could not be saved",
  );
  await page.getByRole("button", { name: "Retry saving login" }).click();
  await expect(page.getByTestId("account-protection")).toBeHidden();
});

test("new accounts can open the existing encrypted backup flow or defer it", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("login-tab-register").click();
  await page.getByTestId("login-username").fill("backup-user");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  const notice = page.getByTestId("account-protection");
  await expect(notice).toContainText("recovery key separately");
  await notice.getByRole("button", { name: "Create encrypted backup" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "I’ll do this later" }).click();
  await expect(notice).toBeHidden();
});

for (const theme of ["light", "dark"]) {
  test(`password form contrast remains readable in ${theme} mode`, async ({
    page,
  }) => {
    await page.addInitScript(
      (value) => localStorage.setItem("mesh-talk-theme", value),
      theme,
    );
    await page.goto("/");
    for (const tab of ["signin", "register"]) {
      await page.getByTestId(`login-tab-${tab}`).click();
      const result = await new AxeBuilder({ page })
        .include('[data-testid="login-form"]')
        .withRules(["color-contrast"])
        .analyze();
      expect(result.violations).toEqual([]);
    }
  });
}

for (const language of ["en", "es", "ja", "zh-Hans", "zh-Hant", "yue"]) {
  test(`creation action remains visible in a short ${language} window`, async ({
    page,
  }) => {
    await page.addInitScript(
      (value) => localStorage.setItem("mesh-talk-lang", value),
      language,
    );
    await page.setViewportSize({ width: 760, height: 520 });
    await page.goto("/");
    await page.getByTestId("login-tab-register").click();
    await expect(page.getByTestId("login-submit")).toBeInViewport();
  });
}

test("creation respects declining saved login and explains the next password requirement", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("login-tab-register").click();
  await page.getByTestId("login-stay-signed-in").uncheck();
  await page.getByTestId("login-username").fill("manual-login");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  const notice = page.getByTestId("account-protection");
  await expect(notice).toContainText("Your login is not saved");
  await expect(notice).toContainText("you will need it next time");
  await expect(
    notice.getByRole("button", { name: "Retry saving login" }),
  ).toBeHidden();
});

test("unconfirmed saved-login status never claims that credentials were saved", async ({
  page,
}) => {
  await page.goto("/");
  await page.evaluate(() => {
    const ipc = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args?: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const invoke = ipc.invoke.bind(ipc);
    ipc.invoke = async (cmd, args) => {
      const result = await invoke(cmd, args);
      if (cmd === "login") {
        const compatible = { ...(result as Record<string, unknown>) };
        delete compatible.saved_session;
        return compatible;
      }
      return result;
    };
  });
  await page.getByTestId("login-tab-register").click();
  await page.getByTestId("login-username").fill("unconfirmed-login");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await expect(page.getByTestId("account-protection")).toContainText(
    "Saved-login status could not be confirmed",
  );
});

test("failed and rejected credential retries stay actionable until saving succeeds", async ({
  page,
}) => {
  await page.goto("/");
  await page.evaluate(() => {
    const ipc = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args?: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const invoke = ipc.invoke.bind(ipc);
    let retries = 0;
    ipc.invoke = async (cmd, args) => {
      if (cmd === "retry_saved_session") {
        retries++;
        if (retries === 1) return "failed";
        if (retries === 2) throw new Error("credential provider unavailable");
      }
      const result = await invoke(cmd, args);
      return cmd === "login"
        ? { ...(result as object), saved_session: "failed" }
        : result;
    };
  });
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  const notice = page.getByTestId("account-protection");
  const retry = notice.getByRole("button", { name: "Retry saving login" });
  for (let attempt = 0; attempt < 2; attempt++) {
    await retry.click();
    await expect(notice.getByRole("alert")).toContainText(
      "Could not save your login",
    );
    await expect(retry).toBeEnabled();
    await expect(page.getByTestId("chat-shell")).toBeVisible();
  }
  await retry.click();
  await expect(notice).toBeHidden();
});
