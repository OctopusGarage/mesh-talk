import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
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
    const indexAudit = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21aa", "wcag22aa"])
      .analyze();
    expect(indexAudit.violations).toEqual([]);
    await page.locator("#results a").click();
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(
      "Change where received files are saved",
    );
    await expect(page.locator("article")).toContainText("Download folder");
    const contents = page.getByRole("navigation", { name: "On this page" });
    await expect(contents).toBeVisible();
    const sectionLink = contents.getByRole("link").first();
    const anchor = await sectionLink.getAttribute("href");
    await sectionLink.click();
    await expect(page.locator(anchor!)).toBeInViewport();
    for (const colorScheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme });
      const audit = await new AxeBuilder({ page })
        .withTags(["wcag2a", "wcag2aa", "wcag21aa", "wcag22aa"])
        .analyze();
      expect(audit.violations).toEqual([]);
    }
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

test("help search supports shareable queries, clearing, history and retry", async ({
  page,
}) => {
  await page.goto(`${origin}index.html?q=下载目录`);
  await expect(page.getByLabel("Search help")).toHaveValue("下载目录");
  await expect(page.locator("#results li")).toHaveCount(1);
  await page.getByRole("button", { name: "Clear search" }).click();
  await expect(page.getByLabel("Search help")).toHaveValue("");
  await expect(page).not.toHaveURL(/\?q=/);
  await page.keyboard.press("Escape");
  await page.locator("h1").click();
  await page.keyboard.press("/");
  await expect(page.getByLabel("Search help")).toBeFocused();
  await page.getByLabel("Search help").fill("下载目录");
  await page.locator("#results a").click();
  await page.goBack();
  await expect(page.getByLabel("Search help")).toHaveValue("下载目录");
  await expect(page.locator("#results li")).toHaveCount(1);
  await page.route("**/index.json", (route) =>
    route.fulfill({ status: 503, body: "Unavailable" }),
  );
  await page.reload();
  await expect(page.locator("#status")).toContainText("unavailable");
  await page.unroute("**/index.json");
  await page.getByRole("button", { name: "Retry search" }).click();
  await expect(page.locator("#results li")).toHaveCount(1);
});

test("help lookup has a bounded timeout and keeps static topics available", async ({
  page,
}) => {
  await page.route("**/index.json", () => {});
  await page.clock.install();
  await page.goto(`${origin}index.html`);
  await expect(page.locator("#status")).toContainText("Loading search");
  await page.clock.fastForward(8100);
  await expect(page.locator("#status")).toContainText("unavailable");
  await expect(page.locator("#results li")).not.toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Retry search" }),
  ).toBeVisible();
});

test("reader language follows the site while articles explicitly remain English", async ({
  page,
}) => {
  await page.goto(`${origin}index.html?lang=zh-Hans`);
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hans");
  await expect(page.getByRole("heading", { level: 1 })).not.toHaveText(
    "Mesh-Talk help",
  );
  await page
    .locator("#results a", { hasText: "Change where received files are saved" })
    .click();
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hans");
  await expect(page.locator("article")).toHaveAttribute("lang", "en");
  await expect(page.locator(".help-language-note")).toBeVisible();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(
    "Change where received files are saved",
  );
  await page.goto(`${origin}v/${version}/index.html?lang=ja`);
  await expect(page.locator("html")).toHaveAttribute("lang", "ja");
  await page.locator("#search").fill("下载目录");
  await expect(page.locator("#results li")).toHaveCount(1);
});

for (const language of ["pt-BR", "ko"]) {
  test(`${language} readers translate navigation and disclose English articles at every version depth`, async ({
    page,
  }) => {
    const messages = JSON.parse(
      await readFile(
        join(checkout, "site/locales", `${language}.json`),
        "utf8",
      ),
    );
    for (const prefix of ["", `v/${version}/`]) {
      await page.goto(`${origin}${prefix}index.html?lang=${language}`);
      await expect(page.locator("html")).toHaveAttribute("lang", language);
      await expect(page.locator("#site-language")).toHaveValue(language);
      if (language === "pt-BR")
        await expect(
          page.locator('#site-language option[value="pt-BR"]'),
        ).toHaveText("Português");
      await expect(
        page.getByRole("searchbox", { name: messages["Search help"] }),
      ).toHaveAttribute("id", "search");
      await page
        .locator("#results a", {
          hasText: "Change where received files are saved",
        })
        .click();
      await expect(page.locator("html")).toHaveAttribute("lang", language);
      await expect(page.locator("article")).toHaveAttribute("lang", "en");
      await expect(page.locator(".help-language-note")).toBeVisible();
      await expect(page.locator(".help-language-note")).toContainText(
        messages[
          "This guide is written in English. Navigation and search follow your language."
        ],
      );
    }
  });
}
