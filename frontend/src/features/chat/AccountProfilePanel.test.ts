// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AccountProfilePanel } from "./AccountProfilePanel";
import { useAuth } from "@/store/auth";
import type { AccountProfileView } from "@/lib/api/accountProfile";
const mocks = vi.hoisted(() => ({ get: vi.fn(), saveBio: vi.fn() }));
vi.mock("@/lib/api/accountProfile", () => ({ accountProfile: mocks }));
let root: Root;
let host: HTMLDivElement;
function card(account: string): AccountProfileView {
  return {
    account_id: account,

    display_name: account,
    bio: account,
    created_at_ms: null,
  };
}
beforeEach(() => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  mocks.get.mockReset();
  mocks.saveBio.mockReset();
  useAuth.setState({
    user: {
      id: "owner",
      username: "tester",
      display_name: "Tester",
    },
    generation: 1,
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
it("discards a card response when its contact has changed", async () => {
  let finish!: (value: AccountProfileView) => void;
  mocks.get
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValueOnce(card("bob"));
  await act(async () =>
    root.render(createElement(AccountProfilePanel, { accountId: "alice" })),
  );
  await act(async () =>
    root.render(createElement(AccountProfilePanel, { accountId: "bob" })),
  );
  await act(async () => finish(card("alice")));
  expect(
    host.querySelector('[data-testid="account-profile-details"]')?.textContent,
  ).toContain("bob");
  expect(
    host.querySelector('[data-testid="account-profile-details"]')?.textContent,
  ).not.toContain("alice");
});
it("discards a response after the local session generation retires", async () => {
  let finish!: (value: AccountProfileView) => void;
  mocks.get
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockImplementationOnce(() => new Promise(() => {}));
  await act(async () =>
    root.render(createElement(AccountProfilePanel, { accountId: "alice" })),
  );
  await act(async () => {
    useAuth.setState({ generation: 2 });
    finish(card("alice"));
  });
  expect(
    host.querySelector('[data-testid="account-profile-details"]'),
  ).toBeNull();
});
it("edits only About and keeps creation metadata read-only", async () => {
  mocks.get.mockResolvedValue({
    ...card("self"),
    created_at_ms: Date.UTC(2026, 0, 1),
  });
  mocks.saveBio.mockResolvedValue(undefined);
  await act(async () =>
    root.render(
      createElement(AccountProfilePanel, { accountId: "self", own: true }),
    ),
  );
  expect(host.querySelector("input")).toBeNull();
  expect(host.textContent).not.toMatch(
    /signed|witness|introduction|JSON|publish/i,
  );
  const area = host.querySelector("textarea")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(area, "Hello");
    area.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const save = Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent === "Save",
  );
  await act(async () => save!.click());
  expect(mocks.saveBio).toHaveBeenCalledWith("owner", "Hello");
});
it("shows unknown dates without offering to supply one and keeps contacts read-only", async () => {
  mocks.get.mockResolvedValue(card("bob"));
  await act(async () =>
    root.render(createElement(AccountProfilePanel, { accountId: "bob" })),
  );
  expect(host.textContent).toContain("Unknown");
  expect(host.querySelector("textarea,input")).toBeNull();
});
it("does not apply a save response to a replacement contact", async () => {
  let finish!: () => void;
  mocks.get
    .mockResolvedValueOnce(card("alice"))
    .mockResolvedValueOnce(card("bob"));
  mocks.saveBio.mockImplementation(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  await act(async () =>
    root.render(
      createElement(AccountProfilePanel, { accountId: "alice", own: true }),
    ),
  );
  const area = host.querySelector("textarea")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(area, "Updated Alice");
    area.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () =>
    Array.from(host.querySelectorAll("button"))
      .find((b) => b.textContent === "Save")!
      .click(),
  );
  await act(async () =>
    root.render(
      createElement(AccountProfilePanel, { accountId: "bob", own: true }),
    ),
  );
  await act(async () => finish());
  expect(host.querySelector("textarea")!.value).toBe("bob");
  expect(host.textContent).not.toContain("Profile saved");
});
it("keeps a failed save draft editable for correction and retry", async () => {
  mocks.get.mockResolvedValue(card("self"));
  mocks.saveBio.mockRejectedValue(new Error("Could not save"));
  await act(async () =>
    root.render(
      createElement(AccountProfilePanel, { accountId: "self", own: true }),
    ),
  );
  const area = host.querySelector("textarea")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(area, "New bio");
    area.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () =>
    Array.from(host.querySelectorAll("button"))
      .find((b) => b.textContent === "Save")!
      .click(),
  );
  expect(host.querySelector("textarea")!.disabled).toBe(false);
  expect(host.querySelector("textarea")!.value).toBe("New bio");
  expect(
    Array.from(host.querySelectorAll("button")).some(
      (button) => button.textContent === "Retry",
    ),
  ).toBe(false);
});
it("accepts exactly 2048 UTF-8 bytes and explains oversized multilingual bios before saving", async () => {
  mocks.get.mockResolvedValue(card("self"));
  await act(async () =>
    root.render(
      createElement(AccountProfilePanel, { accountId: "self", own: true }),
    ),
  );
  const area = host.querySelector("textarea")!;
  const change = async (value: string) =>
    act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLTextAreaElement.prototype,
        "value",
      )!.set!.call(area, value);
      area.dispatchEvent(new Event("input", { bubbles: true }));
    });
  const save = () =>
    Array.from(host.querySelectorAll("button")).find(
      (b) => b.textContent === "Save",
    )!;
  await change("界".repeat(682) + "ab");
  expect(save().disabled).toBe(false);
  await change("界".repeat(682) + "abc");
  expect(save().disabled).toBe(true);
  expect(area.getAttribute("aria-invalid")).toBe("true");
  expect(host.querySelector('[role="alert"]')?.textContent).toContain("2048");
  expect(area.value).toBe("界".repeat(682) + "abc");
});
it("keeps contact details aligned with a newer live profile when the initial read finishes late", async () => {
  const { useAccountProfiles } = await import("@/store/accountProfiles");
  useAccountProfiles.getState().clear();
  let finish!: (value: AccountProfileView) => void;
  mocks.get
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValueOnce({ ...card("bob"), bio: "New live bio" });
  await act(async () =>
    root.render(createElement(AccountProfilePanel, { accountId: "bob" })),
  );
  await act(async () => useAccountProfiles.getState().invalidate("bob"));
  await act(async () => finish({ ...card("bob"), bio: "Outdated bio" }));
  expect(host.textContent).toContain("New live bio");
  expect(host.textContent).not.toContain("Outdated bio");
});
it("keeps an unsaved own About draft when a live profile arrives", async () => {
  const { useAccountProfiles } = await import("@/store/accountProfiles");
  useAccountProfiles.getState().clear();
  mocks.get
    .mockResolvedValueOnce(card("self"))
    .mockResolvedValueOnce({ ...card("self"), bio: "Sibling edit" });
  await act(async () =>
    root.render(
      createElement(AccountProfilePanel, { accountId: "self", own: true }),
    ),
  );
  const area = host.querySelector("textarea")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(area, "Unsaved draft");
    area.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => useAccountProfiles.getState().invalidate("self"));
  expect(area.value).toBe("Unsaved draft");
});
it("does not replace a successful live profile with an obsolete request failure", async () => {
  const { useAccountProfiles } = await import("@/store/accountProfiles");
  useAccountProfiles.getState().clear();
  let fail!: (reason: Error) => void;
  mocks.get
    .mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          fail = reject;
        }),
    )
    .mockResolvedValueOnce({ ...card("bob"), bio: "Live bio" });
  await act(async () =>
    root.render(createElement(AccountProfilePanel, { accountId: "bob" })),
  );
  await act(async () => useAccountProfiles.getState().invalidate("bob"));
  await act(async () => fail(new Error("Obsolete network error")));
  expect(host.textContent).toContain("Live bio");
  expect(host.querySelector('[role="alert"]')).toBeNull();
});

it("restarts a pending profile load after recovered identity invalidates the cache", async () => {
  const { useAccountProfiles } = await import("@/store/accountProfiles");
  useAccountProfiles.getState().clear();
  let finish!: (value: AccountProfileView) => void;
  mocks.get
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValueOnce({ ...card("bob"), bio: "Recovered profile" });
  await act(async () =>
    root.render(createElement(AccountProfilePanel, { accountId: "bob" })),
  );
  await act(async () => {
    useAccountProfiles.getState().clear();
    finish({ ...card("bob"), bio: "Retired profile" });
  });
  expect(mocks.get).toHaveBeenCalledTimes(2);
  expect(host.textContent).toContain("Recovered profile");
  expect(host.textContent).not.toContain("Retired profile");
  expect(
    host.querySelector('[data-testid="account-profile-details"]'),
  ).not.toBeNull();
});
