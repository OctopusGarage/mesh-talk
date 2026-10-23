// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
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
  await i18n.changeLanguage("en");
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: mocks.copy.mockResolvedValue(undefined) },
  });
  access = {
    enabled: false,
    running: false,
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
  expect(mocks.set).toHaveBeenLastCalledWith("alice", true, [destination]);
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
  const details = host.querySelector("details")!;
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
