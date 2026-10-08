import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("a second touch does not take over a sidebar resize", async ({ page }) => {
  await enterChat(page);
  const handle = page.getByTestId("sidebar-resize-handle");
  const box = await handle.boundingBox();
  if (!box) throw new Error("Missing sidebar resize handle");
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  const session = await page.context().newCDPSession(page);
  const point = (id: number, clientX: number) => ({
    id,
    x: clientX,
    y,
    radiusX: 4,
    radiusY: 4,
  });
  const touch = (
    type: "touchStart" | "touchMove" | "touchEnd",
    points: ReturnType<typeof point>[],
  ) => session.send("Input.dispatchTouchEvent", { type, touchPoints: points });

  await touch("touchStart", [point(1, x)]);
  await touch("touchMove", [point(1, x + 40)]);
  const firstWidth = Number(await handle.getAttribute("aria-valuenow"));
  expect(firstWidth).toBeGreaterThan(284);

  await touch("touchStart", [point(1, x + 40), point(2, x + 130)]);
  await touch("touchMove", [point(1, x + 40), point(2, x + 150)]);
  await expect(handle).toHaveAttribute("aria-valuenow", String(firstWidth));

  await touch("touchMove", [point(1, x + 56), point(2, x + 150)]);
  await expect(handle).toHaveAttribute(
    "aria-valuenow",
    String(firstWidth + 16),
  );
  await touch("touchEnd", [point(2, x + 150)]);
  await touch("touchMove", [point(2, x + 170)]);
  await expect(handle).toHaveAttribute(
    "aria-valuenow",
    String(firstWidth + 16),
  );
  await touch("touchEnd", []);

  const moved = await handle.boundingBox();
  if (!moved) throw new Error("Missing sidebar resize handle after drag");
  const nextX = moved.x + moved.width / 2;
  await touch("touchStart", [point(3, nextX)]);
  await touch("touchMove", [point(3, nextX + 16)]);
  await touch("touchEnd", []);
  const finalWidth = Number(await handle.getAttribute("aria-valuenow"));
  expect(finalWidth).toBeGreaterThan(firstWidth + 16);
  expect(
    await page.evaluate(() => localStorage.getItem("mesh-talk-sidebar-width")),
  ).toBe(String(finalWidth));
});
