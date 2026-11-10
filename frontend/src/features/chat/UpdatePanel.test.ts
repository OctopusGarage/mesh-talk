// @vitest-environment jsdom
import i18n from "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  DownloadedUpdate,
  DownloadProgress,
  UpdateCheck,
} from "@/lib/types";
import { useUpdates } from "@/store/updates";
import { UpdatePanel } from "./UpdatePanel";

const mocks = vi.hoisted(() => ({
  check: vi.fn(),
  download: vi.fn(),
  cancel: vi.fn(),
  prepare: vi.fn(),
  install: vi.fn(),
  openUrl: vi.fn(),
  revealItemInDir: vi.fn(),
}));
vi.mock("@/store/updateRestart", () => ({
  prepareUpdateRestart: vi.fn(async () => () => {}),
}));
vi.mock("@/lib/api", () => ({ updates: mocks }));
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: mocks.openUrl,
  revealItemInDir: mocks.revealItemInDir,
}));
const candidate: UpdateCheck = {
  channel: "nightly",
  current_version: "0.4.4-nightly.41.1",
  latest_version: "0.4.4-nightly.42.1",
  is_newer: true,
  release_url:
    "https://github.com/OctopusGarage/mesh-talk/releases/tag/v0.4.4-nightly.42.1",
  notes: "Preview feature for early feedback",
  edition: "default",
  package_name: "mesh-talk_v0.4.4-nightly.42.1_linux_x86_64.zip",
  package_size: 10485760,
};
let root: Root;
let host: HTMLDivElement;
const select = () => host.querySelector<HTMLSelectElement>("select")!;
function button(text: string) {
  const element = Array.from(host.querySelectorAll("button")).find(
    (item) => item.textContent === text,
  );
  if (!element) throw new Error(`Missing button: ${text}`);
  return element;
}
async function click(text: string) {
  await act(async () => button(text).click());
}
async function choose(value: string) {
  await act(async () => {
    select().value = value;
    select().dispatchEvent(new Event("change", { bubbles: true }));
  });
}
beforeEach(async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  vi.resetAllMocks();
  localStorage.clear();
  await i18n.changeLanguage("en");
  useUpdates.setState({
    phase: "idle",
    channel: "stable",
    currentVersion: null,
    release: null,
    download: null,
    progress: null,
    error: null,
    canceling: false,
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => root.render(createElement(UpdatePanel)));
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

it("offers an accessible opt-in channel, persists choice and clears an obsolete release without checking implicitly", async () => {
  expect(select().value).toBe("stable");
  expect(host.querySelector("label")?.htmlFor).toBe(select().id);
  expect(host.textContent).toContain(
    "Switching channels never automatically downgrades",
  );
  await choose("nightly");
  expect(localStorage.getItem("mesh-talk-update-channel")).toBe("nightly");
  expect(host.textContent).toContain("Experimental builds may contain bugs");
  expect(mocks.check).not.toHaveBeenCalled();
  expect(mocks.download).not.toHaveBeenCalled();
  mocks.check.mockResolvedValue(candidate);
  await click("Check for updates");
  expect(mocks.check).toHaveBeenCalledWith("nightly");
  expect(host.textContent).toContain(`Installed: ${candidate.current_version}`);
  expect(host.textContent).toContain(candidate.notes);
  await choose("stable");
  expect(host.textContent).not.toContain(candidate.notes);
  expect(host.querySelector('[data-testid="download-update"]')).toBeNull();
  expect(host.textContent).toContain(`Installed: ${candidate.current_version}`);
  expect(mocks.check).toHaveBeenCalledTimes(1);
  await act(async () => root.render(null));
  await act(async () => root.render(createElement(UpdatePanel)));
  expect(select().value).toBe("stable");
});

it("keeps channel selection disabled during checks and verified downloads, then reveals the downloaded file", async () => {
  let checked!: (release: UpdateCheck) => void;
  let downloaded!: (download: DownloadedUpdate) => void;
  let progress!: (value: DownloadProgress) => void;
  mocks.check.mockReturnValue(
    new Promise((resolve) => {
      checked = resolve;
    }),
  );
  mocks.download.mockImplementation((_version, callback) => {
    progress = callback;
    return new Promise((resolve) => {
      downloaded = resolve;
    });
  });
  await choose("nightly");
  await click("Check for updates");
  expect(select().disabled).toBe(true);
  expect(button("Checking…").disabled).toBe(true);
  await act(async () => checked(candidate));
  expect(select().disabled).toBe(false);
  await click("Download update");
  expect(mocks.download).toHaveBeenCalledWith(
    candidate.latest_version,
    expect.any(Function),
    "nightly",
  );
  expect(select().disabled).toBe(true);
  await act(async () => progress({ downloaded: 5242880, total: 10485760 }));
  expect(host.querySelector("progress")?.value).toBe(5242880);
  expect(host.textContent).toContain("50%");
  await act(async () =>
    progress({
      downloaded: 0,
      total: 0,
      phase: "preparing",
    } as DownloadProgress),
  );
  expect(host.textContent).toContain("Preparing update");
  expect(host.querySelector("progress")?.hasAttribute("value")).toBe(false);
  expect(select().disabled).toBe(true);
  await act(async () =>
    progress({
      downloaded: 0,
      total: 10485760,
      phase: "fallback",
    } as DownloadProgress),
  );
  expect(host.textContent).toContain("Downloading full update");
  expect(host.querySelector("progress")?.value).toBe(0);
  await act(async () =>
    downloaded({
      path: "/Downloads/nightly.zip",
      file_name: "nightly.zip",
    }),
  );
  expect(select().disabled).toBe(false);
  expect(host.textContent).toContain("Download verified");
  expect(host.textContent).toContain("DEB or RPM");
  await click("Show in Downloads");
  expect(mocks.revealItemInDir).toHaveBeenCalledWith("/Downloads/nightly.zip");
  mocks.revealItemInDir.mockRejectedValueOnce(
    new Error("Downloads unavailable"),
  );
  await click("Show in Downloads");
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "Downloads unavailable",
  );
});

it("opens the chosen release and a manual Stable return, surfacing opener failures", async () => {
  await click("Download Stable manually");
  expect(mocks.openUrl).toHaveBeenCalledWith(
    "https://github.com/OctopusGarage/mesh-talk/releases/latest",
  );
  mocks.check.mockResolvedValue(candidate);
  await click("Check for updates");
  await click("View release page");
  expect(mocks.openUrl).toHaveBeenLastCalledWith(candidate.release_url);
  mocks.openUrl.mockRejectedValueOnce(new Error("Browser unavailable"));
  await click("Download Stable manually");
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "Browser unavailable",
  );
});

it("reports a retryable check failure and an up-to-date installed identity", async () => {
  mocks.check.mockRejectedValueOnce(new Error("Network unavailable"));
  await click("Check for updates");
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "Network unavailable",
  );
  mocks.check.mockResolvedValueOnce({ ...candidate, is_newer: false });
  await click("Check for updates");
  expect(host.textContent).toContain(
    `You're up to date (${candidate.current_version}).`,
  );
  expect(host.querySelector('[role="alert"]')).toBeNull();
  expect(host.querySelector('[data-testid="download-update"]')).toBeNull();
});

it("offers one-click updates only for eligible signed releases and shows native installation progress", async () => {
  mocks.check.mockResolvedValue({ ...candidate, automatic_install: true });
  mocks.prepare.mockResolvedValue({
    path: "/private/payload",
    file_name: "app.tar.gz",
    installable: true,
  });
  let finish!: () => void;
  let progress!: (value: "installing" | "restarting") => void;
  mocks.install.mockImplementation((_version, callback) => {
    progress = callback;
    return new Promise<void>((resolve) => {
      finish = resolve;
    });
  });
  await click("Check for updates");
  expect(button("Update and restart").disabled).toBe(false);
  await click("Update and restart");
  expect(host.textContent).toContain("Installing update");
  expect(select().disabled).toBe(true);
  expect(
    host.querySelector('button[data-testid="download-update"]'),
  ).toBeNull();
  await act(async () => progress("restarting"));
  expect(host.textContent).toContain("Restarting Mesh-Talk");
  await act(async () => finish());
});

it("keeps native install failures recoverable with retry and a manual release link", async () => {
  mocks.check.mockResolvedValue({ ...candidate, automatic_install: true });
  mocks.prepare.mockResolvedValue({
    path: "/private/payload",
    file_name: "app.tar.gz",
    installable: true,
  });
  mocks.install.mockRejectedValue(new Error("Authorization canceled"));
  await click("Check for updates");
  await click("Update and restart");
  expect(button("Restart to update").disabled).toBe(false);
  expect(host.textContent).toContain("Authorization canceled");
  expect(host.textContent).not.toContain("DEB or RPM");
  expect(host.textContent).not.toContain("Show in Downloads");
  await click("Download manually");
  expect(mocks.openUrl).toHaveBeenLastCalledWith(candidate.release_url);
});
