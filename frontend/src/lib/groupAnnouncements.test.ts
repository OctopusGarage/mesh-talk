// @vitest-environment jsdom
import { beforeEach, expect, it, vi } from "vitest";
import {
  announcementSeen,
  dismissAnnouncement,
  withAnnouncementPublications,
} from "./groupAnnouncements";
import type { ChannelMetadataValue } from "./types";
beforeEach(() => localStorage.clear());
it("dismisses an exact revision privately without hiding later updates or another account", () => {
  dismissAnnouncement("owner", "account", "group", "one");
  expect(announcementSeen("owner", "account", "group")).toBe("one");
  expect(announcementSeen("owner", "other-account", "group")).toBe("");
  dismissAnnouncement("owner", "account", "group", "two");
  expect(announcementSeen("owner", "account", "group")).toBe("two");
});
it("merges announcement events once without mutating ordinary history", () => {
  const publication: ChannelMetadataValue = {
    revision: "one",
    content: "Notice",
    author: "a",
    authored_at: 1,
  };
  const history = [
    {
      id: "text",
      fromMe: false,
      who: "b",
      text: "Hello",
      wallClock: 2,
      replyTo: null,
    },
  ];
  const result = withAnnouncementPublications(
    history,
    [publication, publication],
    "a",
  );
  expect(result.map((m) => m.id)).toEqual(["announcement:one", "text"]);
  expect(result[0].announcement).toEqual(publication);
  expect(history).toHaveLength(1);
});

it("preserves causal message order even when authored clocks go backwards", () => {
  const history = [20, 10, 10].map((wallClock, index) => ({
    id: `message-${index}`,
    fromMe: false,
    who: "b",
    text: "Text",
    wallClock,
    replyTo: null,
  }));
  const result = withAnnouncementPublications(
    history,
    [{ revision: "one", content: "Notice", author: "a", authored_at: 15 }],
    "a",
  );
  expect(result.filter((entry) => !entry.announcement)).toEqual(history);
});
it("dismisses in-session when storage can be read but writes fail", () => {
  const spy = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
    throw new Error("quota");
  });
  try {
    dismissAnnouncement("quota-owner", "quota-account", "group", "one");
    expect(announcementSeen("quota-owner", "quota-account", "group")).toBe(
      "one",
    );
  } finally {
    spy.mockRestore();
  }
});
