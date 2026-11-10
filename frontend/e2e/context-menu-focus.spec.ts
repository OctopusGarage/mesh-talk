import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, seedTheme } from "./helpers/session";

for (const theme of ["light", "dark"] as const) {
  test(`context menu keyboard focus uses a visible token indicator in ${theme}`, async ({
    page,
  }, info) => {
    await seedTheme(page, theme);
    await enterChat(page);
    await openBobDm(page);
    await page
      .getByTestId("message-bubble")
      .first()
      .locator("[data-context-menu]")
      .click({ button: "right" });
    const menu = page.getByTestId("message-context-menu");
    await expect(menu).toBeVisible();
    const react = page.getByTestId("msg-react");
    const items = menu
      .getByRole("menuitem")
      .and(page.locator(':not([aria-disabled="true"])'));
    await page.mouse.move(0, 0);
    await page.keyboard.press("Home");
    await expect(items.first()).toBeFocused();
    const ids = await items.evaluateAll((elements) =>
      elements.map((el) => el.getAttribute("data-testid")),
    );
    const targetIndex = ids.indexOf("msg-react");
    expect(targetIndex).toBeGreaterThanOrEqual(0);
    for (let i = 1; i <= targetIndex; i++) {
      await page.keyboard.press("ArrowDown");
      await expect(items.nth(i)).toBeFocused();
    }
    await expect(react).toBeFocused();
    const style = await react.evaluate((el) => {
      const css = getComputedStyle(el);
      const probe = document.createElement("span");
      probe.style.color = `hsl(${css.getPropertyValue("--ring")})`;
      document.body.append(probe);
      const ringColor = getComputedStyle(probe).color;
      probe.remove();
      return {
        role: el.getAttribute("role"),
        focusVisible: el.matches(":focus-visible"),
        outline: css.outline,
        outlineStyle: css.outlineStyle,
        outlineWidth: css.outlineWidth,
        outlineColor: css.outlineColor,
        outlineOffset: css.outlineOffset,
        ringColor,
        boxShadow: css.boxShadow,
        background: css.backgroundColor,
        color: css.color,
      };
    });
    // Outline color follows the shared color transition; observe its settled
    // token without treating an intermediate animation sample as the result.
    await expect
      .poll(() => react.evaluate((el) => getComputedStyle(el).outlineColor))
      .toBe(style.ringColor);
    style.outlineColor = await react.evaluate(
      (el) => getComputedStyle(el).outlineColor,
    );
    await info.attach("focus-style", {
      body: JSON.stringify(style),
      contentType: "application/json",
    });
    if (process.env.MESH_CAPTURE_FOCUS)
      await page.screenshot({
        path: info.outputPath(`${theme}-menu-focus.png`),
      });
    expect(style.focusVisible).toBe(true);
    expect(style.outlineStyle).not.toBe("none");
    expect(parseFloat(style.outlineWidth)).toBeGreaterThanOrEqual(2);
    expect(style.outlineColor).toBe(style.ringColor);
    expect(style.outlineOffset).toBe("-2px");
    await page.keyboard.press("Enter");
    await expect(page.getByTestId("reaction-picker")).toBeVisible();
    await expect(page.getByTestId("reaction-picker")).toHaveAttribute(
      "data-state",
      "open",
    );
    await page.getByTestId("reaction-option-🔥").click();
    await expect(page.getByTestId("reaction-chip").first()).toContainText("🔥");
  });
}
