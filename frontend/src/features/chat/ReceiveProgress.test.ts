import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import "@/lib/i18n";
import type { FileStatus } from "@/lib/types";
import { ReceiveProgress } from "./ReceiveProgress";

const render = (status?: FileStatus) =>
  renderToStaticMarkup(createElement(ReceiveProgress, { status }));
const receiving = (done: number, total: number): FileStatus => ({
  file_conv: "file",
  done,
  total,
  ready: false,
});
const renderImage = (
  status?: FileStatus,
  options: { size?: number; preparing?: boolean } = {},
) =>
  renderToStaticMarkup(
    createElement(ReceiveProgress, {
      status,
      presentation: "image",
      ...options,
    }),
  );

it.each([
  [0, 0],
  [-1, 10],
  [1, -1],
  [Number.NaN, 10],
  [1, Number.POSITIVE_INFINITY],
  [Number.POSITIVE_INFINITY, 10],
])("keeps unknown or invalid counts %s/%s indeterminate", (done, total) => {
  const html = render(receiving(done, total));
  expect(html).toContain('role="progressbar"');
  expect(html).not.toContain("aria-valuenow");
  expect(html).not.toContain("%");
  expect(html).not.toMatch(/NaN|Infinity/);
  expect(html).toContain("scaleX(0)");
});

it.each([
  [0, 0, 0],
  [4, 40, 0.4],
  [10, 100, 1],
  [12, 100, 1],
])("bounds rendered progress for %s of ten chunks", (done, percent, ratio) => {
  const html = render(receiving(done, 10));
  expect(html).toContain(`aria-valuenow="${percent}"`);
  expect(html).toContain(`>${percent}%</span>`);
  expect(html).toContain(`scaleX(${ratio})`);
});

it("keeps missing status indeterminate and ready attachments hidden", () => {
  expect(render()).not.toContain("aria-valuenow");
  expect(render({ ...receiving(10, 10), ready: true })).toBe("");
});

it("waits for the first chunk without claiming an active transfer", () => {
  const html = render(receiving(0, 10));
  expect(html).toContain("Waiting for file…");
  expect(html).not.toContain("Receiving file");
  expect(html).toContain('aria-valuenow="0"');
  expect(html).toContain(">0%</span>");
});

it("shows receiving after a chunk has arrived", () => {
  const html = render(receiving(1, 10));
  expect(html).toContain("Receiving file…");
  expect(html).not.toContain("Waiting for file…");
  expect(html).toContain('aria-valuenow="10"');
});

it.each(["pending", "declined"] as const)(
  "does not display byte progress while consent is %s",
  (consent) => {
    const html = render({ ...receiving(4, 10), consent });
    expect(html).toContain('role="status"');
    expect(html).not.toContain('role="progressbar"');
    expect(html).not.toContain("%");
  },
);

it.each([
  undefined,
  receiving(0, 0),
  receiving(-1, 10),
  receiving(1, -1),
  receiving(Number.NaN, 10),
  receiving(1, Number.POSITIVE_INFINITY),
  receiving(Number.POSITIVE_INFINITY, 10),
])(
  "waits quietly for an image with unknown or invalid counts: %j",
  (status) => {
    const html = renderImage(status);
    expect(html).toContain("Waiting for image");
    expect(html).toContain('role="progressbar"');
    expect(html).not.toContain("aria-valuenow");
    expect(html).not.toMatch(/%|NaN|Infinity|animate-spin|stroke-dashoffset/);
    expect(html).toContain('width="24"');
    expect(html).not.toContain("mt-2");
  },
);

it("keeps a known image zero accessible without a visible percentage or ring", () => {
  const html = renderImage(receiving(0, 4));
  expect(html).toContain("Waiting for image");
  expect(html).toContain('aria-valuenow="0"');
  expect(html).not.toMatch(/0%|animate-spin|stroke-dashoffset/);
});

it("shows measured image progress with a ring and advertised size", () => {
  const html = renderImage(receiving(3, 4), { size: 1_500_000 });
  expect(html).toContain("Receiving image · 75%");
  expect(html).toContain('aria-valuenow="75"');
  expect(html).toContain('width="48"');
  expect(html).toContain('stroke-width="3"');
  expect(html).toContain("Photo · 1.4 MB");
  expect(html).not.toMatch(/<button|animate-spin/);
});

it("announces only the image phase while the visible percentage changes", () => {
  const announcement = (html: string) =>
    html.match(/<span[^>]*role="status"[^>]*>(.*?)<\/span>/)?.[0];
  const first = announcement(renderImage(receiving(1, 4)));
  const next = announcement(renderImage(receiving(3, 4)));
  expect(first).toBeDefined();
  expect(first).toContain('aria-live="polite"');
  expect(first).toContain("Receiving image");
  expect(first).not.toContain("%");
  expect(next).toBe(first);
});

it.each([4, 6])(
  "prepares an image after %s of four chunks before ready",
  (done) => {
    const html = renderImage(receiving(done, 4));
    expect(html).toContain("Preparing image");
    expect(html).not.toMatch(
      /%|role="progressbar"|animate-spin|stroke-dashoffset/,
    );
  },
);

it("prepares a ready image until the preview can be displayed", () => {
  const html = renderImage({ ...receiving(4, 4), ready: true });
  expect(html).toContain("Preparing image");
  expect(html).not.toMatch(/role="progressbar"|complete|displaying/i);
});

it("prepares an available image when transfer status is still missing", () => {
  const html = renderImage(undefined, { preparing: true });
  expect(html).toContain("Preparing image");
  expect(html).not.toMatch(/Waiting for image|role="progressbar"/);
});

it.each([
  [{ unavailable: true }, "Attachment unavailable"],
  [
    { consent: "pending" as const },
    "Waiting for your approval to receive this attachment.",
  ],
  [{ consent: "declined" as const }, "You declined this attachment."],
])(
  "prioritizes terminal image state %j over preparation",
  (terminal, label) => {
    const html = renderImage(
      { ...receiving(4, 4), ready: true, ...terminal },
      { preparing: true },
    );
    expect(html).toContain(label);
    expect(html).not.toMatch(
      /Preparing image|role="progressbar"|%|stroke-dashoffset/,
    );
  },
);

it.each([undefined, 0, -1, Number.NaN, Number.POSITIVE_INFINITY])(
  "omits invalid advertised image size %s",
  (size) => {
    const html = renderImage(receiving(1, 4), { size });
    expect(html).toContain(">Photo</p>");
    expect(html).not.toMatch(/Photo ·|NaN|Infinity/);
  },
);

it("limits image progress motion to a reduced-motion-aware stroke transition", () => {
  const html = renderImage(receiving(3, 4));
  expect(html).toContain("transition-[stroke-dashoffset]");
  expect(html).toContain("duration-[160ms]");
  expect(html).toContain("motion-reduce:transition-none");
  expect(html).not.toContain("animate-");
});
