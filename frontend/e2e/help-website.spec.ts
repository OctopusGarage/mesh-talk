import { test, expect } from "@playwright/test";
import { createServer, type Server } from "node:http";
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, extname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

let directory: string;
let server: Server;
let origin: string;
let version: string;
const checkout = fileURLToPath(new URL("../../", import.meta.url));
test.beforeAll(async () => {
  directory = await mkdtemp(join(tmpdir(), "mesh-talk-help-browser-"));
  execFileSync(process.execPath, [
    join(checkout, "tools/help/build.mjs"),
    "--out",
    directory,
  ]);
  version = JSON.parse(
    await readFile(join(directory, "version.json"), "utf8"),
  ).version;
  server = createServer(async (request, response) => {
    try {
      const pathname = decodeURIComponent(
        new URL(request.url ?? "/", "http://localhost").pathname,
      );
      const relative = pathname.replace(/^\/mesh-talk\/help\//, "");
      const file = resolve(directory, relative);
      if (
        !pathname.startsWith("/mesh-talk/help/") ||
        !file.startsWith(directory + sep)
      )
        throw Error("not found");
      const types: Record<string, string> = {
        ".html": "text/html",
        ".json": "application/json",
        ".mjs": "text/javascript",
        ".css": "text/css",
      };
      response.setHeader(
        "Content-Type",
        `${types[extname(file)] ?? "text/plain"}; charset=utf-8`,
      );
      response.end(await readFile(file));
    } catch {
      response.writeHead(404).end("Not found");
    }
  });
  await new Promise<void>((done, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", done);
  });
  const address = server.address();
  if (!address || typeof address === "string")
    throw Error("Missing test server address");
  origin = `http://127.0.0.1:${address.port}/mesh-talk/help/`;
});
test.afterAll(async () => {
  if (server)
    await new Promise<void>((done, reject) =>
      server.close((error) => (error ? reject(error) : done())),
    );
  if (directory) await rm(directory, { recursive: true, force: true });
});
for (const width of [1440, 390]) {
  test(`online manual preserves project paths and readability at ${width}`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(`${origin}index.html`);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(
      "Mesh-Talk help",
    );
    await expect(page.locator("#status")).toContainText(/\d+ topics/);
    if (width === 390)
      await expect(page.locator("details")).not.toHaveAttribute("open", "");
    await page.getByLabel("Search help").fill("下载目录");
    await expect(page.locator("#results li")).toHaveCount(1);
    await page.locator("#results a").click();
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(
      "Change where received files are saved",
    );
    await expect(page.locator("article")).toContainText("Download folder");
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth - innerWidth,
      ),
    ).toBeLessThanOrEqual(1);
    if (process.env.MESH_TALK_HELP_SCREENSHOTS === "1") {
      await mkdir(join(checkout, ".impeccable/review"), { recursive: true });
      await page.screenshot({
        path: join(checkout, `.impeccable/review/help-online-${width}.png`),
        fullPage: true,
      });
    }
    await page.goto(`${origin}v/${version}/index.html`);
    await page.getByLabel("Search help").fill("zzzz-no-topic");
    await expect(page.locator("#status")).toContainText("No topics match");
    const markdown = await page.request.get(`${origin}v/${version}/backup.md`);
    expect(markdown.ok()).toBe(true);
    expect(await markdown.text()).toContain(
      "# Back up and restore your account",
    );
  });
}

test("static manual remains navigable without JavaScript", async ({
  browser,
}) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  try {
    const page = await context.newPage();
    await page.goto(`${origin}index.html`);
    await page
      .locator("#results a", { hasText: "Back up and restore your account" })
      .click();
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(
      "Back up and restore your account",
    );
    await page.getByRole("link", { name: "Read as Markdown" }).click();
    await expect(page.locator("body")).toContainText(
      "# Back up and restore your account",
    );
  } finally {
    await context.close();
  }
});
