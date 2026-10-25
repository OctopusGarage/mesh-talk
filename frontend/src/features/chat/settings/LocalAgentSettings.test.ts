// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { useContactPolicy } from "@/store/contactPolicy";
import type { LocalAgentAccess } from "@/lib/api/localAgentAccess";
import type { SettingsController } from "./useSettingsController";
const mocks = vi.hoisted(() => ({ get: vi.fn(), set: vi.fn(), copy: vi.fn() }));
vi.mock("@/lib/api/localAgentAccess", () => ({ localAgentAccess: mocks }));
import { LocalAgentSettings } from "./LocalAgentSettings";
let host: HTMLDivElement;
let root: Root;
let controller: SettingsController;
const destination = { kind: "channel" as const, id: "ab".repeat(32) };
let access: LocalAgentAccess;
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetAllMocks();
  useContactPolicy.getState().reset();
  useContactPolicy.setState({ owner: "alice", loaded: true });
  await i18n.changeLanguage("en");
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: mocks.copy.mockResolvedValue(undefined) },
  });
  access = {
    enabled: false,
    running: false,
    lifetime: "one_hour",
    expires_at_ms: null,
    expired: false,
    recent_activity: [],
    activity_error: null,
    destinations: [],
    available_destinations: [{ destination, label: "Release team" }],
    instructions_path: "/home/user/.codex/AGENTS.md",
    instructions: "",
    error: null,
  };
  mocks.get.mockImplementation(async () => access);
  mocks.set.mockImplementation(async (_owner, enabled, destinations) => {
    access = {
      ...access,
      enabled,
      running: enabled,
      destinations,
      instructions: enabled
        ? "Mesh-Talk local API: http://127.0.0.1:12345/v1/help"
        : "",
    };
    return access;
  });
  controller = {
    identity: "alice:1",
    owner: "alice",
    settings: {},
    saving: false,
    runSettingAction: async (
      _category: string,
      write: () => Promise<LocalAgentAccess>,
      apply: (value: LocalAgentAccess) => void,
    ) => {
      try {
        apply(await write());
        return true;
      } catch {
        return false;
      }
    },
  } as unknown as SettingsController;
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
async function render() {
  await act(async () =>
    root.render(createElement(LocalAgentSettings, { controller })),
  );
}
async function search(value: string) {
  const input = host.querySelector<HTMLInputElement>('input[type="search"]')!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
it("requires an explicit conversation before enabling and sends the exact selected scope", async () => {
  await render();
  expect(
    host.querySelector<HTMLButtonElement>("#setting-local-agent-access")!
      .disabled,
  ).toBe(true);
  await act(async () =>
    host.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click(),
  );
  expect(mocks.set).toHaveBeenLastCalledWith("alice", false, [destination]);
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>("#setting-local-agent-access")!
      .click(),
  );
  expect(mocks.set).toHaveBeenLastCalledWith("alice", true, [destination], {
    lifetime: "one_hour",
  });
  expect(host.textContent).toContain("Running");
});
it("discloses and copies the same short help bootstrap without exposing credentials", async () => {
  access = {
    ...access,
    enabled: true,
    running: true,
    destinations: [destination],
    instructions: "Mesh-Talk local API: http://127.0.0.1:12345/v1/help",
  };
  await render();
  const details = host.querySelector<HTMLDetailsElement>(
    '[data-testid="agent-instructions"]',
  )!;
  expect(details.open).toBe(false);
  details.open = true;
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="copy-agent-instructions"]',
      )!
      .click(),
  );
  expect(mocks.copy).toHaveBeenCalledWith(access.instructions);
  expect(host.querySelector("pre")!.textContent).toBe(access.instructions);
  expect(host.textContent).not.toContain("Bearer");
});
it("offers retry after a load failure", async () => {
  mocks.get.mockRejectedValueOnce(new Error("Node not ready"));
  await render();
  expect(host.querySelector('[role="alert"]')!.textContent).toContain(
    "Node not ready",
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="retry-agent-access"]')!
      .click(),
  );
  expect(host.querySelector("#setting-local-agent-access")).not.toBeNull();
});
it("reloads canonical readiness after a failed mutation", async () => {
  access = {
    ...access,
    enabled: true,
    running: true,
    destinations: [destination],
    instructions: "help",
  };
  await render();
  mocks.set.mockImplementationOnce(async () => {
    access = { ...access, enabled: false, running: false, instructions: "" };
    throw new Error("Instructions could not be removed");
  });
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>("#setting-local-agent-access")!
      .click(),
  );
  expect(
    host
      .querySelector("#setting-local-agent-access")!
      .getAttribute("aria-checked"),
  ).toBe("false");
  expect(mocks.get).toHaveBeenCalledTimes(2);
});

it("discards a delayed load from a replaced settings session", async () => {
  let finish!: (value: LocalAgentAccess) => void;
  mocks.get.mockReturnValueOnce(
    new Promise<LocalAgentAccess>((resolve) => {
      finish = resolve;
    }),
  );
  await render();
  const stale = {
    ...access,
    enabled: true,
    running: true,
    instructions: "stale account help",
  };
  controller = { ...controller, identity: "bob:2", owner: "bob" };
  await render();
  await act(async () => {
    finish(stale);
  });
  expect(host.textContent).not.toContain("stale account help");
  expect(
    host
      .querySelector("#setting-local-agent-access")!
      .getAttribute("aria-checked"),
  ).toBe("false");
});

it("announces clipboard failure and lets the user retry the exact bootstrap", async () => {
  access = {
    ...access,
    enabled: true,
    running: true,
    destinations: [destination],
    instructions: "help bootstrap",
  };
  mocks.copy.mockRejectedValueOnce(new Error("Clipboard denied"));
  await render();
  const copy = host.querySelector<HTMLButtonElement>(
    '[data-testid="copy-agent-instructions"]',
  )!;
  await act(async () => copy.click());
  expect(host.querySelector('[role="alert"]')!.textContent).toBe(
    i18n.t("agentAccess.copyFailed"),
  );
  await act(async () => copy.click());
  expect(mocks.copy).toHaveBeenLastCalledWith("help bootstrap");
  expect(host.querySelector('[role="alert"]')).toBeNull();
  expect(copy.textContent).toContain(i18n.t("agentAccess.copied"));
});

it("keeps an unavailable selected conversation removable and disables its final grant", async () => {
  access = {
    ...access,
    enabled: true,
    running: false,
    destinations: [destination],
    available_destinations: [],
    error: "Conversation unavailable",
  };
  await render();
  const selected = host.querySelector<HTMLInputElement>(
    'input[type="checkbox"]',
  )!;
  expect(selected.checked).toBe(true);
  expect(host.querySelector('[role="alert"]')!.textContent).toBe(
    "Conversation unavailable",
  );
  await act(async () => selected.click());
  expect(mocks.set).toHaveBeenLastCalledWith("alice", false, []);
  expect(
    host.querySelector<HTMLInputElement>('input[type="checkbox"]'),
  ).toBeNull();
  expect(host.textContent).toContain(i18n.t("agentAccess.noConversations"));
});

it("preserves the original mutation error when canonical reload also fails", async () => {
  access = {
    ...access,
    enabled: true,
    running: true,
    destinations: [destination],
  };
  await render();
  mocks.set.mockRejectedValueOnce(new Error("Could not retire instructions"));
  mocks.get.mockRejectedValueOnce(new Error("Node stopped"));
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>("#setting-local-agent-access")!
      .click(),
  );
  expect(host.querySelector('[role="alert"]')!.textContent).toContain(
    "Could not retire instructions",
  );
  expect(host.textContent).not.toContain("Node stopped");
});

it("discards mutation completion after the settings identity changes", async () => {
  access = { ...access, destinations: [destination] };
  await render();
  let finish!: (value: LocalAgentAccess) => void;
  mocks.set.mockReturnValueOnce(
    new Promise<LocalAgentAccess>((resolve) => {
      finish = resolve;
    }),
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>("#setting-local-agent-access")!
      .click(),
  );
  controller = { ...controller, identity: "bob:2", owner: "bob" };
  access = { ...access, enabled: false, running: false, destinations: [] };
  await render();
  await act(async () =>
    finish({
      ...access,
      enabled: true,
      running: true,
      instructions: "stale mutation help",
    }),
  );
  expect(host.textContent).not.toContain("stale mutation help");
  expect(
    host
      .querySelector("#setting-local-agent-access")!
      .getAttribute("aria-checked"),
  ).toBe("false");
});

it("retries an unavailable enabled service with its exact account scope", async () => {
  const person = { kind: "account" as const, id: "acc_bob_bbbb2222" };
  access = {
    ...access,
    enabled: true,
    running: false,
    destinations: [person],
    available_destinations: [{ destination: person, label: "Bob" }],
    error: "Service startup failed",
  };
  await render();
  const retry = [...host.querySelectorAll<HTMLButtonElement>("button")].find(
    (button) => button.textContent === i18n.t("contactVisibility.retry"),
  )!;
  await act(async () => retry.click());
  expect(mocks.set).toHaveBeenLastCalledWith("alice", true, [person]);
  expect(host.querySelector('[role="status"]')!.textContent).toContain(
    i18n.t("agentAccess.running"),
  );
  expect(host.textContent).toContain(i18n.t("agentAccess.person"));
});

it("keeps a failed reload actionable and clears its error after a successful retry", async () => {
  mocks.get.mockRejectedValueOnce(new Error("Initial startup failure"));
  await render();
  mocks.get.mockRejectedValueOnce(new Error("Still unavailable"));
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="retry-agent-access"]')!
      .click(),
  );
  expect(host.querySelector('[role="alert"]')!.textContent).toContain(
    "Still unavailable",
  );
  expect(
    host.querySelector<HTMLButtonElement>('[data-testid="retry-agent-access"]')!
      .disabled,
  ).toBe(false);
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="retry-agent-access"]')!
      .click(),
  );
  expect(host.querySelector('[role="alert"]')).toBeNull();
  expect(host.querySelector("#setting-local-agent-access")).not.toBeNull();
});

it("discards a failed reload after its settings identity retires", async () => {
  mocks.get.mockRejectedValueOnce(new Error("Initial failure"));
  await render();
  let fail!: (reason: Error) => void;
  mocks.get.mockReturnValueOnce(
    new Promise<LocalAgentAccess>((_resolve, reject) => {
      fail = reject;
    }),
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="retry-agent-access"]')!
      .click(),
  );
  controller = { ...controller, identity: "bob:2", owner: "bob" };
  await render();
  await act(async () => fail(new Error("Alice reload failed")));
  expect(host.querySelector('[role="alert"]')).toBeNull();
  expect(host.textContent).not.toContain("Alice reload failed");
  expect(mocks.get).toHaveBeenLastCalledWith("bob");
});

for (const outcome of ["success", "failure"] as const) {
  it(`ignores ${outcome} clipboard completion from a retired settings identity`, async () => {
    access = {
      ...access,
      enabled: true,
      running: true,
      destinations: [destination],
      instructions: "Alice bootstrap",
    };
    let finish!: () => void;
    mocks.copy.mockReturnValueOnce(
      new Promise<void>((resolve, reject) => {
        finish =
          outcome === "success"
            ? resolve
            : () => reject(new Error("Clipboard unavailable"));
      }),
    );
    await render();
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>(
          '[data-testid="copy-agent-instructions"]',
        )!
        .click(),
    );
    controller = { ...controller, identity: "bob:2", owner: "bob" };
    access = { ...access, instructions: "Bob bootstrap" };
    await render();
    await act(async () => finish());
    expect(host.querySelector('[role="alert"]')).toBeNull();
    expect(
      host.querySelector('[data-testid="copy-agent-instructions"]')!
        .textContent,
    ).toContain(i18n.t("agentAccess.copy"));
    expect(host.textContent).not.toContain(i18n.t("agentAccess.copied"));
    expect(host.querySelector("pre")!.textContent).toBe("Bob bootstrap");
  });
}

it("does not publish an old owner's reload or error after its mutation fails", async () => {
  access = { ...access, destinations: [destination] };
  await render();
  let fail!: (reason: Error) => void;
  mocks.set.mockReturnValueOnce(
    new Promise<LocalAgentAccess>((_resolve, reject) => {
      fail = reject;
    }),
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>("#setting-local-agent-access")!
      .click(),
  );
  controller = { ...controller, identity: "bob:2", owner: "bob" };
  access = { ...access, destinations: [] };
  await render();
  mocks.get.mockResolvedValueOnce({
    ...access,
    enabled: true,
    running: true,
    instructions: "Alice stale reload",
  });
  await act(async () => fail(new Error("Alice write failed")));
  expect(mocks.get).toHaveBeenLastCalledWith("alice");
  expect(host.querySelector('[role="alert"]')).toBeNull();
  expect(host.textContent).not.toContain("Alice stale reload");
  expect(
    host
      .querySelector("#setting-local-agent-access")!
      .getAttribute("aria-checked"),
  ).toBe("false");
});

it("does not load private settings before an owner is available", async () => {
  controller = { ...controller, identity: "signed-out:2", owner: "" };
  await render();
  expect(mocks.get).not.toHaveBeenCalled();
  expect(
    host.querySelector<HTMLButtonElement>("#setting-local-agent-access")!
      .disabled,
  ).toBe(true);
});

it("omits hidden accounts from new scopes while keeping channels available", async () => {
  const account = { kind: "account" as const, id: "bob" };
  access.available_destinations.push({ destination: account, label: "Bob" });
  useContactPolicy.setState({
    contacts: { bob: { account_id: "bob", name: "Bob" } },
  });
  await render();
  expect(host.textContent).not.toContain("Bob");
  expect(host.textContent).toContain("Release team");
  expect(mocks.set).not.toHaveBeenCalled();
});
it.each([
  { owner: "alice", loaded: false },
  { owner: "other", loaded: true },
])("waits for the current owner's contact policy: %j", async (policy) => {
  access.available_destinations.push({
    destination: { kind: "account", id: "bob" },
    label: "Bob",
  });
  useContactPolicy.setState(policy);
  await render();
  expect(host.textContent).not.toContain("Bob");
  expect(host.textContent).toContain("Release team");
  await act(async () =>
    useContactPolicy.setState({ owner: "alice", loaded: true }),
  );
  expect(host.textContent).toContain("Bob");
});
it("keeps a hidden granted scope manageable until explicitly revoked", async () => {
  const account = { kind: "account" as const, id: "bob" };
  access.available_destinations.push({ destination: account, label: "Bob" });
  access.destinations = [account];
  await render();
  await act(async () =>
    useContactPolicy.setState({
      contacts: { bob: { account_id: "bob", name: "Bob" } },
    }),
  );
  const checkbox = Array.from(
    host.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'),
  ).find((input) => input.parentElement?.textContent?.includes("Bob"))!;
  expect(checkbox.checked).toBe(true);
  expect(mocks.set).not.toHaveBeenCalled();
  await act(async () => checkbox.click());
  expect(mocks.set).toHaveBeenLastCalledWith("alice", false, []);
  expect(host.textContent).not.toContain("Bob");
});

it("keeps a large roster collapsed and searches without changing existing grants", async () => {
  access.available_destinations = Array.from({ length: 300 }, (_, index) => ({
    destination: { kind: index % 2 ? "channel" : "account", id: String(index) },
    label: index === 299 ? "喵星球" : `Contact ${index}`,
  }));
  access.destinations = [access.available_destinations[0].destination];
  await render();
  const picker = host.querySelector<HTMLDetailsElement>(
    '[data-testid="agent-conversation-picker"]',
  )!;
  expect(picker).not.toBeNull();
  expect(picker.open).toBe(false);
  expect(picker.querySelector("summary")!.textContent).toContain(
    "1 / 16 selected",
  );
  picker.open = true;
  await search("  喵星球  ");
  expect(picker.querySelectorAll('input[type="checkbox"]')).toHaveLength(1);
  await act(async () =>
    picker.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click(),
  );
  expect(mocks.set).toHaveBeenLastCalledWith("alice", false, [
    access.available_destinations[0].destination,
    access.available_destinations[299].destination,
  ]);
  await search("CONTACT 298");
  expect(picker.querySelectorAll('input[type="checkbox"]')).toHaveLength(1);
  await search("no-such-conversation");
  expect(picker.textContent).toContain("No matching conversations.");
  expect(picker.querySelectorAll('input[type="checkbox"]')).toHaveLength(0);
  await search("");
  expect(picker.querySelectorAll('input[type="checkbox"]')).toHaveLength(300);
});

it("filters to selected conversations and allows removing an unavailable grant at the limit", async () => {
  access.destinations = Array.from({ length: 16 }, (_, index) => ({
    kind: "account",
    id: String(index),
  }));
  await render();
  expect(
    host.querySelector<HTMLInputElement>('input[type="checkbox"]')!.disabled,
  ).toBe(true);
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="agent-selected-only"]')!
      .click(),
  );
  expect(host.querySelectorAll('input[type="checkbox"]')).toHaveLength(16);
  const checkbox = host.querySelector<HTMLInputElement>(
    'input[type="checkbox"]',
  )!;
  expect(checkbox.disabled).toBe(false);
  await act(async () => checkbox.click());
  expect(mocks.set.mock.lastCall![2]).toHaveLength(15);
  expect(host.querySelectorAll('input[type="checkbox"]')).toHaveLength(15);
});

it("reports writes to Integrations and exposes setup without expanding technical details", async () => {
  access = {
    ...access,
    enabled: true,
    running: true,
    destinations: [destination],
    instructions: "public help",
  };
  const action = vi.spyOn(controller, "runSettingAction");
  await render();
  const details = host.querySelector<HTMLDetailsElement>(
    '[data-testid="agent-instructions"]',
  )!;
  expect(details.open).toBe(false);
  expect(
    details.contains(
      host.querySelector('[data-testid="copy-agent-instructions"]'),
    ),
  ).toBe(false);
  expect(details.textContent).toContain(access.instructions_path);
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="copy-agent-instructions"]',
      )!
      .click(),
  );
  expect(mocks.copy).toHaveBeenCalledWith("public help");
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>("#setting-local-agent-access")!
      .click(),
  );
  expect(action.mock.lastCall![0]).toBe("integrations");
});

it("shows saving locally instead of claiming the previous service status is current", async () => {
  access = {
    ...access,
    enabled: true,
    running: true,
    destinations: [destination],
  };
  await render();
  controller = { ...controller, saving: true };
  await render();
  expect(
    host.querySelector('[data-testid="agent-access-status"]')!.textContent,
  ).toContain("Saving");
  expect(
    host.querySelector<HTMLButtonElement>("#setting-local-agent-access")!
      .disabled,
  ).toBe(true);
});

it("defaults fresh authorization to one hour and makes persistent access explicit", async () => {
  access.destinations = [destination];
  await render();
  const duration = host.querySelector<HTMLSelectElement>(
    "#agent-access-duration",
  )!;
  expect(duration.value).toBe("one_hour");
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>("#setting-local-agent-access")!
      .click(),
  );
  expect(mocks.set).toHaveBeenLastCalledWith("alice", true, [destination], {
    lifetime: "one_hour",
  });
});
it("preserves existing persistent access and renews only on explicit request", async () => {
  access = {
    ...access,
    enabled: true,
    running: true,
    lifetime: "persistent",
    destinations: [destination],
  };
  await render();
  expect(host.textContent).toContain(i18n.t("agentAccess.persistent"));
  expect(mocks.set).not.toHaveBeenCalled();
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="renew-agent-access"]')!
      .click(),
  );
  expect(mocks.set).toHaveBeenLastCalledWith("alice", true, [destination], {
    lifetime: "persistent",
    renew: true,
  });
});
it("refreshes expiration read-only and keeps expired state distinct", async () => {
  vi.useFakeTimers();
  try {
    vi.setSystemTime(new Date("2026-10-25T00:00:00Z"));
    access = {
      ...access,
      enabled: true,
      running: true,
      destinations: [destination],
      expires_at_ms: Date.now() + 1000,
    };
    await render();
    expect(host.querySelector("time")?.dateTime).toBe(
      "2026-10-25T00:00:01.000Z",
    );
    access = { ...access, running: false, expired: true };
    await act(async () => vi.advanceTimersByTimeAsync(1001));
    expect(host.textContent).toContain(i18n.t("agentAccess.expired"));
    expect(mocks.get).toHaveBeenCalledTimes(2);
    expect(mocks.set).not.toHaveBeenCalled();
  } finally {
    vi.useRealTimers();
  }
});
it("shows only safe activity metadata and distinguishes failed history from empty history", async () => {
  access.recent_activity = [
    { at_ms: 1792886400000, destination, action: "retried" },
  ];
  await render();
  const activity = host.querySelector('[data-testid="agent-access-activity"]')!;
  expect(activity.textContent).toContain("Release team");
  expect(activity.textContent).toContain(i18n.t("agentAccess.activityRetried"));
  expect(activity.textContent).toContain(
    i18n.t("agentAccess.activityDeliveryHint"),
  );
  access = {
    ...access,
    recent_activity: [],
    activity_error: "Audit unavailable",
  };
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="refresh-agent-activity"]',
      )!
      .click(),
  );
  expect(activity.textContent).toContain(i18n.t("agentAccess.activityFailed"));
  expect(activity.textContent).not.toContain(
    i18n.t("agentAccess.activityEmpty"),
  );
  expect(mocks.set).not.toHaveBeenCalled();
});
it("does not renew after a cancelled verification", async () => {
  access = {
    ...access,
    enabled: true,
    expired: true,
    destinations: [destination],
    expires_at_ms: 1,
  };
  await render();
  mocks.set.mockRejectedValueOnce({
    kind: "authentication-canceled",
    message: "Verification canceled",
  });
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="renew-agent-access"]')!
      .click(),
  );
  expect(host.textContent).toContain(i18n.t("agentAccess.expired"));
  expect(mocks.set).toHaveBeenCalledTimes(1);
  expect(mocks.get).toHaveBeenCalledTimes(1);
  expect(host.querySelector('[role="alert"]')).toBeNull();
});

it("does not apply a duration draft during retry or ordinary scope edits", async () => {
  access = {
    ...access,
    enabled: true,
    running: false,
    destinations: [destination],
    expires_at_ms: Date.now() + 60000,
  };
  await render();
  await act(async () => {
    const select = host.querySelector<HTMLSelectElement>(
      "#agent-access-duration",
    )!;
    select.value = "persistent";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  const retry = [...host.querySelectorAll<HTMLButtonElement>("button")].find(
    (button) => button.textContent === i18n.t("contactVisibility.retry"),
  )!;
  await act(async () => retry.click());
  expect(mocks.set).toHaveBeenLastCalledWith("alice", true, [destination]);
  await act(async () =>
    host.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click(),
  );
  expect(mocks.set).toHaveBeenLastCalledWith("alice", false, []);
});
it("discards activity refresh after only its owner changes", async () => {
  await render();
  let finish!: (value: LocalAgentAccess) => void;
  mocks.get.mockReturnValueOnce(
    new Promise<LocalAgentAccess>((resolve) => {
      finish = resolve;
    }),
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="refresh-agent-activity"]',
      )!
      .click(),
  );
  controller = { ...controller, owner: "bob" };
  await render();
  await act(async () =>
    finish({
      ...access,
      recent_activity: [
        {
          at_ms: 1,
          destination: { kind: "account", id: "Alice private destination" },
          action: "accepted",
        },
      ],
    }),
  );
  expect(host.textContent).not.toContain("Alice private destination");
});
it("cleans the expiry refresh timer on unmount", async () => {
  vi.useFakeTimers();
  try {
    access = {
      ...access,
      enabled: true,
      running: true,
      expires_at_ms: Date.now() + 1000,
    };
    await render();
    await act(async () => root.render(null));
    await act(async () => vi.advanceTimersByTimeAsync(2000));
    expect(mocks.get).toHaveBeenCalledTimes(1);
  } finally {
    vi.useRealTimers();
  }
});
