import { test, expect } from "@playwright/test";
import { servePublicSite } from "./helpers/public-site";

let hosted: Awaited<ReturnType<typeof servePublicSite>>;
test.use({ javaScriptEnabled: false });
test.beforeAll(async () => {
  hosted = await servePublicSite();
});
test.afterAll(async () => {
  await hosted?.close();
});

test("published HTML exposes working AI discovery without JavaScript", async ({
  page,
  request,
}) => {
  const base = new URL("./", hosted.url).href;
  await page.goto(hosted.url);
  await expect(
    page.locator('link[rel="alternate"][type="text/markdown"]'),
  ).toHaveAttribute("href", "index.md");
  await expect(page.locator('link[rel="describedby"]')).toHaveAttribute(
    "href",
    "llms.txt",
  );
  const schema = JSON.parse(
    (await page.locator('script[type="application/ld+json"]').textContent()) ??
      "{}",
  );
  expect(schema.name).toBe("Mesh-Talk");
  expect(schema.softwareVersion).toBeUndefined();
  for (const name of ["index.md", "llms.txt", "llms-full.txt", "sitemap.xml"]) {
    const response = await request.get(base + name);
    expect(response.status(), name).toBe(200);
    expect(await response.text()).not.toContain("<!doctype html>");
  }
  const markdown = await request.get(base + "index.md");
  expect(markdown.headers()["content-type"]).toContain("text/markdown");
  expect(await markdown.text()).toContain("You create a local account");
  await page.goto(base + "help/about-mesh-talk.html");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(
    "About Mesh-Talk",
  );
  await expect(page.locator('link[rel="canonical"]')).toHaveAttribute(
    "href",
    "https://octopusgarage.github.io/mesh-talk/help/about-mesh-talk.html",
  );
  await expect(page.locator('meta[name="description"]')).toHaveAttribute(
    "content",
    /local accounts/,
  );
  await expect(page.locator("article")).toContainText(
    "not guaranteed permanent storage",
  );
  const version = await (await request.get(base + "help/version.json")).json();
  await page.goto(base + `help/v/${version.version}/about-mesh-talk.html`);
  await expect(page.locator('link[rel="canonical"]')).toHaveAttribute(
    "href",
    `https://octopusgarage.github.io/mesh-talk/help/v/${version.version}/about-mesh-talk.html`,
  );
});
