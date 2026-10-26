import { test, expect } from "./tauri-mock";
import { BOB, enterChat, openBobDm } from "./helpers/session";

/** The real header and call dialogs with a controlled backend roster. */
async function prepare(
  page: import("@playwright/test").Page,
  direct: boolean | null,
) {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(
    async ({ account, direct }) => {
      const chatPath = "/src/store/chat.ts",
        settingsPath = "/src/store/settings.ts";
      const { useChat } = await import(/* @vite-ignore */ chatPath);
      const { useSettings } = await import(/* @vite-ignore */ settingsPath);
      const state = { capture: 0, signals: 0 };
      (window as unknown as { __callReadiness: typeof state }).__callReadiness =
        state;
      navigator.mediaDevices.getUserMedia = async () => {
        state.capture++;
        throw new DOMException("Test microphone denied", "NotAllowedError");
      };
      const internals = (
        window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (cmd: string, args: unknown) => Promise<unknown>;
          };
        }
      ).__TAURI_INTERNALS__;
      const invoke = internals.invoke;
      internals.invoke = (cmd, args) => {
        if (cmd === "send_call_signal") state.signals++;
        return invoke(cmd, args);
      };
      useSettings.setState({ callsEnabled: true });
      useChat.setState({
        peers: useChat.getState().peers.map((peer: { account_id: string }) =>
          peer.account_id === account
            ? {
                ...peer,
                directly_reachable: direct === null ? undefined : direct,
                discovered_on_lan: false,
              }
            : peer,
        ),
      });
    },
    { account: BOB.account, direct },
  );
}

test("relay-only voice and video explain the limitation before requesting capture", async ({
  page,
}) => {
  await prepare(page, false);
  for (const kind of ["voice", "video"]) {
    await page.getByTestId(`conversation-call-${kind}`).click();
    await expect(page.getByRole("dialog")).toContainText(
      "Calling unavailable on this connection",
    );
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(page.getByTestId(`conversation-call-${kind}`)).toBeFocused();
  }
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as {
            __callReadiness: { capture: number; signals: number };
          }
        ).__callReadiness,
    ),
  ).toEqual({ capture: 0, signals: 0 });
});

for (const direct of [true, null]) {
  test(`${direct ? "routed direct" : "legacy unknown"} route attempts a call and keeps capture failure actionable`, async ({
    page,
  }) => {
    await prepare(page, direct);
    await page.getByTestId("conversation-call-voice").click();
    await expect(page.getByRole("dialog")).toContainText(
      "Couldn’t use the microphone",
    );
    await expect(page.getByTestId("call-retry")).toBeVisible();
    const before = await page.evaluate(
      () =>
        (window as unknown as { __callReadiness: { capture: number } })
          .__callReadiness.capture,
    );
    await page.getByTestId("call-retry").click();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __callReadiness: { capture: number } })
              .__callReadiness.capture,
        ),
      )
      .toBeGreaterThan(before);
    await page.getByTestId("call-check-connection").click();
    await expect(page.getByTestId("diagnostics-dialog")).toBeVisible();
    await expect(
      page.getByRole("tab", { name: "Troubleshoot" }),
    ).toHaveAttribute("data-state", "active");
  });
}
