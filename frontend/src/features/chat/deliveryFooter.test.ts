import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import i18n from "@/lib/i18n";
import { MessageBubble } from "./MessageBubble";
import type { ChatMessage } from "@/store/chat";

const base: ChatMessage = {
  id: "stable",
  clientId: "client",
  fromMe: true,
  who: "me",
  text: "text",
  wallClock: 1,
  replyTo: null,
  delivery: "awaiting",
};
const render = (m: ChatMessage, isChannel = false, isPersonal = false) =>
  renderToStaticMarkup(
    createElement(MessageBubble, {
      m,
      parent: null,
      showAuthor: false,
      isChannel,
      isPersonal,
      reactions: [],
      selfReactionId: "me",
      myName: "Me",
      onReply: () => {},
      onReact: () => {},
      onRetry: () => {},
      onDelete: () => {},
      onRecall: () => {},
      onReEdit: () => {},
    }),
  );

describe("automatic delivery footer", () => {
  it("visibly identifies a received file offer instead of claiming file delivery", async () => {
    await i18n.changeLanguage("en");
    const html = render({
      ...base,
      delivery: "delivered",
      file: {
        fileConv: "file",
        name: "photo.png",
        size: 12,
        mime: "image/png",
        media: true,
      },
    });
    expect(html).toContain('aria-label="File offer received"');
    expect(html).toContain('aria-hidden="true">File offer received</span>');
    expect(html).not.toContain('aria-label="Delivered"');
  });
  it("distinguishes personal file metadata from bytes and external upload progress", async () => {
    await i18n.changeLanguage("en");
    const html = render(
      {
        ...base,
        file: {
          fileConv: "personal-file",
          name: "note.txt",
          size: 12,
          mime: "text/plain",
          media: false,
        },
      },
      false,
      true,
    );
    expect(html).toContain("File card saved on this device");
    expect(html).not.toContain('role="progressbar"');
    expect(html).not.toContain("data-delivery=");
  });
  it("labels personal messages as locally saved without claiming external delivery", async () => {
    await i18n.changeLanguage("en");
    const html = render(base, false, true);
    expect(html).toContain("Saved on this device");
    expect(html).not.toContain("data-delivery=");
    expect(render({ ...base, pending: true }, false, true)).not.toContain(
      "Saved on this device",
    );
    expect(render({ ...base, failed: true }, false, true)).not.toContain(
      "Saved on this device",
    );
  });
  it("renders an awaiting disclosure button and static delivered receipt for text, sticker and file cards", async () => {
    await i18n.changeLanguage("en");
    for (const m of [
      base,
      { ...base, sticker: "unknown", text: "🙂" },
      {
        ...base,
        metadataPending: true,
        file: {
          name: "file.png",
          size: 0,
          mime: "",
          fileConv: "",
          media: true,
        },
      },
    ]) {
      expect(render(m)).toContain('data-delivery="awaiting"');
      expect(render({ ...m, delivery: "delivered" })).toContain(
        'data-delivery="delivered"',
      );
      expect(render(m)).toContain('data-testid="delivery-details-trigger"');
      expect(render(m)).toContain('aria-haspopup="dialog"');
      expect(render({ ...m, delivery: "delivered" })).toContain('tabindex="0"');
    }
  });
  it("never shows a receipt claim for groups, incoming or untracked messages", () => {
    expect(render(base, true)).not.toContain("data-delivery=");
    expect(render({ ...base, fromMe: false })).not.toContain("data-delivery=");
    expect(render({ ...base, delivery: undefined })).not.toContain(
      "data-delivery=",
    );
  });
  it("shows channel custody separately from delivery", async () => {
    await i18n.changeLanguage("en");
    for (const copies of [0, 1, 2]) {
      const markup = render({ ...base, replicas: copies }, true);
      expect(markup).toContain(`data-custody="${copies}"`);
      expect(markup).not.toContain("data-delivery=");
      expect(markup).toContain("not a delivery or read receipt");
      if (copies > 0) expect(markup).toContain("Last confirmed");
      else expect(markup).toContain("No member copy confirmed");
    }
  });
  it("has explicit automatic-receipt wording in all six locales and hides guessed file metadata", async () => {
    for (const language of ["en", "es", "ja", "yue", "zh-Hans", "zh-Hant"]) {
      for (const key of [
        "awaiting",
        "delivered",
        "help",
        "fileHelp",
        "fileAwaiting",
        "fileDelivered",
        "metadataPending",
        "detailsTitle",
        "attempted",
        "retryScheduled",
        "noDetails",
        "automaticRetry",
        "awaitingFileHelp",
      ])
        expect(
          i18n.getResource(language, "translation", `message.delivery.${key}`),
        ).toBeTruthy();
      await i18n.changeLanguage(language);
      const markup = render({
        ...base,
        metadataPending: true,
        file: {
          name: "photo.png",
          size: 0,
          mime: "",
          fileConv: "",
          media: true,
        },
      });
      expect(markup).not.toContain("0 B");
      expect(markup).not.toContain('data-testid="file-image"');
      expect(markup).toContain('disabled=""');
    }
    await i18n.changeLanguage("en");
  });
});

it("keeps authored time distinct from local receipt detail for recovered content", async () => {
  await i18n.changeLanguage("en");
  const html = render(
    { ...base, fromMe: false, arrivalKind: "catch_up", receivedAt: 500000 },
    true,
  );
  expect(html).toContain("Received later");
  expect(html).toContain("Received on this device:");
  expect(html).toContain('data-testid="message-time"');
  expect(html).not.toContain("synchronized");
  expect(
    render({ ...base, fromMe: false, receivedAt: 500000 }, true),
  ).not.toContain("Received later");
});

it("shows a countdown for each expiring message, including grouped incoming messages", async () => {
  await i18n.changeLanguage("en");
  const html = render({
    ...base,
    fromMe: false,
    expiresAtMs: Date.now() + 299_000,
  });
  expect(html).toContain('data-testid="message-expiry"');
  expect(html).toMatch(/4:5[89]/);
});

it("shows confirmed post-office custody without claiming member delivery", async () => {
  await i18n.changeLanguage("en");
  const html = render({ ...base, replicas: 0, postOfficeCopies: 1 }, true);
  expect(html).toContain('data-custody="1"');
  expect(html).toContain("Post office confirmed a stored copy");
  expect(html).not.toContain("No member copy confirmed");
  expect(html).not.toContain("sending…");
});

it("excludes details for recalled, pending, failed and unaccepted messages", () => {
  for (const message of [
    { ...base, recalled: true },
    { ...base, pending: true },
    { ...base, failed: true },
    { ...base, id: null },
  ]) {
    expect(render(message)).not.toContain(
      'data-testid="delivery-details-trigger"',
    );
  }
});
