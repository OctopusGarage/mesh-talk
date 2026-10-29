import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { selectSettingsCategory } from "./helpers/settings";

// Real Chromium capture and WebRTC, with generated media hardware and substituted
// desktop IPC. This does not establish native permissions or physical devices.
test.use({
  launchOptions: {
    args: [
      "--use-fake-device-for-media-stream",
      "--use-fake-ui-for-media-stream",
    ],
  },
  permissions: ["microphone", "camera"],
});

type Probe = {
  streams: MediaStream[];
  constraints: MediaStreamConstraints[];
  denyCamera: boolean;
};
async function prepare(page: import("@playwright/test").Page) {
  await page.addInitScript(() => {
    const w = window as unknown as { __deviceProbe: Probe };
    w.__deviceProbe = { streams: [], constraints: [], denyCamera: false };
    const capture = navigator.mediaDevices.getUserMedia.bind(
      navigator.mediaDevices,
    );
    navigator.mediaDevices.getUserMedia = async (constraints) => {
      w.__deviceProbe.constraints.push(constraints ?? {});
      if (w.__deviceProbe.denyCamera && constraints?.video)
        throw new DOMException("Camera permission denied", "NotAllowedError");
      const stream = await capture(constraints);
      w.__deviceProbe.streams.push(stream);
      return stream;
    };
  });
  await enterChat(page);
  await page.getByTestId("sidebar-overflow").click();
  await expect(page.getByTestId("sidebar-action-webrtc-test")).toHaveCount(0);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "notifications");
  await page.locator("#setting-calls").click();
  await expect(page.getByTestId("media-device-check")).toBeVisible();
}
async function liveTracks(page: import("@playwright/test").Page) {
  return page.evaluate(
    () =>
      (window as unknown as { __deviceProbe: Probe }).__deviceProbe.streams
        .flatMap((stream) => stream.getTracks())
        .filter((track) => track.readyState === "live").length,
  );
}

test("call settings check real generated devices and bounded local media without a More utility", async ({
  page,
}, info) => {
  await prepare(page);
  const panel = page.getByTestId("media-device-check");
  expect(await liveTracks(page)).toBe(0);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __deviceProbe: Probe }).__deviceProbe
          .constraints.length,
    ),
  ).toBe(0);
  await page.getByTestId("media-check-audio").click();
  await expect(page.getByTestId("media-check-level")).toBeVisible();
  await expect.poll(() => liveTracks(page)).toBe(1);
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as { __deviceProbe: Probe }
        ).__deviceProbe.streams[0].getVideoTracks().length,
    ),
  ).toBe(0);
  await page.getByTestId("media-check-advanced").locator("summary").click();
  await page.getByTestId("media-check-loopback").click();
  await expect(page.getByTestId("media-check-diagnostic-status")).toContainText(
    "Media reception verified",
    { timeout: 15000 },
  );
  await page.getByTestId("media-check-stop").click();
  await expect.poll(() => liveTracks(page)).toBe(0);
  await page.getByTestId("media-check-video").click();
  const preview = page.getByTestId("media-check-camera-preview");
  await expect(preview).toBeVisible();
  await expect
    .poll(() =>
      preview.evaluate((element) => (element as HTMLVideoElement).videoWidth),
    )
    .toBeGreaterThan(0);
  expect(
    await preview.evaluate((element) => (element as HTMLVideoElement).muted),
  ).toBe(true);
  const advanced = page.getByTestId("media-check-advanced");
  if (
    !(await advanced.evaluate(
      (element) => (element as HTMLDetailsElement).open,
    ))
  )
    await advanced.locator("summary").click();
  await page.getByTestId("media-check-loopback").click();
  await expect(page.getByTestId("media-check-diagnostic-status")).toContainText(
    "Media reception verified",
    { timeout: 15000 },
  );
  await page.setViewportSize({ width: 1280, height: 900 });
  await panel.scrollIntoViewIfNeeded();
  await page.screenshot({
    path: info.outputPath("media-device-check-desktop.png"),
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await preview.scrollIntoViewIfNeeded();

  await expect
    .poll(async () => {
      const button = await page
        .getByTestId("settings-nav-notifications")
        .boundingBox();
      const navigation = await page
        .getByRole("navigation", { name: "Settings categories" })
        .boundingBox();
      return (
        !!button &&
        !!navigation &&
        button.x >= navigation.x - 1 &&
        button.x + button.width <= navigation.x + navigation.width + 1
      );
    })
    .toBe(true);
  const box = await page.getByTestId("settings-dialog").boundingBox();
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(390);
  await page.screenshot({
    path: info.outputPath("media-device-check-narrow.png"),
  });
  await selectSettingsCategory(page, "general");
  await expect.poll(() => liveTracks(page)).toBe(0);
  await selectSettingsCategory(page, "notifications");
  expect(await liveTracks(page)).toBe(0);
});

test("camera rejection keeps microphone check available and closing settings stops capture", async ({
  page,
}) => {
  await prepare(page);
  await page.evaluate(() => {
    (window as unknown as { __deviceProbe: Probe }).__deviceProbe.denyCamera =
      true;
  });
  await page.getByTestId("media-check-video").click();
  await expect(page.getByTestId("media-check-error")).toBeVisible();
  expect(await liveTracks(page)).toBe(0);
  await page.getByTestId("media-check-audio").click();
  await expect(page.getByTestId("media-check-level")).toBeVisible();
  await expect.poll(() => liveTracks(page)).toBe(1);
  await page
    .getByTestId("settings-dialog")
    .getByRole("button", { name: "Close", exact: true })
    .click();
  await expect.poll(() => liveTracks(page)).toBe(0);
});
