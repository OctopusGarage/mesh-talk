import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("same-identity disable/re-enable never returns a revoked URL", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(async () => {
    const path = "/e2e/media-hook-harness.tsx";
    const { mountMediaProbe } = await import(/* @vite-ignore */ path);
    const w = window as unknown as Record<string, unknown>;
    const internals = w.__TAURI_INTERNALS__ as {
      invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
    };
    const original = internals.invoke;
    const state = {
      available: true,
      revoked: [] as string[],
      created: [] as string[],
      calls: [] as string[],
    };
    const create = URL.createObjectURL.bind(URL),
      revoke = URL.revokeObjectURL.bind(URL);
    URL.createObjectURL = (blob) => {
      const url = create(blob);
      state.created.push(url);
      return url;
    };
    URL.revokeObjectURL = (url) => {
      state.revoked.push(url);
      revoke(url);
    };
    internals.invoke = async (cmd, args) => {
      if (
        (cmd === "read_media" || cmd === "read_file") &&
        args.fileConv === "hook-probe"
      ) {
        state.calls.push(cmd);
        if (!state.available) throw new Error("missing bytes");
        return new ArrayBuffer(1);
      }
      return original(cmd, args);
    };
    const container = document.createElement("div");
    document.body.append(container);
    const probe = mountMediaProbe(container);
    w.__mediaLifecycle = { state, probe };
    probe.enable(true);
  });
  await expect(page.getByTestId("media-hook-url")).not.toHaveAttribute(
    "data-url",
    "",
  );
  const observations = await page.evaluate(() => {
    const { state, probe } = (
      window as unknown as {
        __mediaLifecycle: {
          state: { available: boolean; revoked: string[]; created: string[] };
          probe: { enable: (enabled: boolean) => void };
        };
      }
    ).__mediaLifecycle;
    const before = document
      .querySelector('[data-testid="media-hook-url"]')!
      .getAttribute("data-url");
    probe.enable(false);
    state.available = false;
    probe.enable(true);
    return {
      before,
      after: document
        .querySelector('[data-testid="media-hook-url"]')!
        .getAttribute("data-url"),
      revoked: state.revoked,
    };
  });
  expect(observations.revoked).toEqual([observations.before]);
  expect(observations.after).toBe("");
  await page.evaluate(() => {
    const { probe } = (
      window as unknown as {
        __mediaLifecycle: { probe: { dispose: () => void } };
      }
    ).__mediaLifecycle;
    probe.dispose();
  });
});

test("cleanup during an admitted durable read prevents fallback and blob creation", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(async () => {
    const path = "/e2e/media-hook-harness.tsx";
    const { mountMediaProbe } = await import(/* @vite-ignore */ path);
    const w = window as unknown as Record<string, unknown>;
    const internals = w.__TAURI_INTERNALS__ as {
      invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
    };
    const original = internals.invoke;
    const state = { calls: [] as string[], blobs: 0, reject: (_: Error) => {} };
    const create = URL.createObjectURL.bind(URL);
    URL.createObjectURL = (blob) => {
      state.blobs++;
      return create(blob);
    };
    internals.invoke = async (cmd, args) => {
      if (
        (cmd === "read_media" || cmd === "read_file") &&
        args.fileConv === "hook-probe"
      ) {
        state.calls.push(cmd);
        return new Promise((_, reject) => {
          state.reject = reject;
        });
      }
      return original(cmd, args);
    };
    const container = document.createElement("div");
    document.body.append(container);
    const probe = mountMediaProbe(container);
    w.__pendingMedia = { probe, state };
    probe.enable(true);
  });
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as unknown as {
              __pendingMedia: { state: { calls: string[] } };
            }
          ).__pendingMedia.state.calls.length,
      ),
    )
    .toBe(1);
  const result = await page.evaluate(async () => {
    const { state, probe } = (
      window as unknown as {
        __pendingMedia: {
          state: { calls: string[]; blobs: number; reject: (e: Error) => void };
          probe: { logout: () => Promise<void>; dispose: () => void };
        };
      }
    ).__pendingMedia;
    await probe.logout();
    probe.dispose();
    state.reject(new Error("late native completion"));
    await Promise.resolve();
    await Promise.resolve();
    return { calls: state.calls, blobs: state.blobs };
  });
  expect(result).toEqual({ calls: ["read_media"], blobs: 0 });
});

test("same UUID relogin revokes the old owned URL exactly once", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(async () => {
    const path = "/e2e/media-hook-harness.tsx";
    const { mountMediaProbe } = await import(/* @vite-ignore */ path);
    const w = window as unknown as Record<string, unknown>;
    const internals = w.__TAURI_INTERNALS__ as {
      invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
    };
    const original = internals.invoke;
    const state = { revoked: [] as string[], created: [] as string[] };
    const create = URL.createObjectURL.bind(URL),
      revoke = URL.revokeObjectURL.bind(URL);
    URL.createObjectURL = (blob) => {
      const url = create(blob);
      state.created.push(url);
      return url;
    };
    URL.revokeObjectURL = (url) => {
      state.revoked.push(url);
      revoke(url);
    };
    internals.invoke = async (cmd, args) =>
      args.fileConv === "hook-probe" &&
      (cmd === "read_media" || cmd === "read_file")
        ? new ArrayBuffer(1)
        : original(cmd, args);
    const container = document.createElement("div");
    document.body.append(container);
    const probe = mountMediaProbe(container);
    w.__reloginMedia = { probe, state };
    probe.enable(true);
  });
  await expect(page.getByTestId("media-hook-url")).not.toHaveAttribute(
    "data-url",
    "",
  );
  const old = await page.getByTestId("media-hook-url").getAttribute("data-url");
  await page.evaluate(async () => {
    const { probe } = (
      window as unknown as {
        __reloginMedia: { probe: { relogin: () => Promise<void> } };
      }
    ).__reloginMedia;
    await probe.relogin();
  });
  await expect
    .poll(() => page.getByTestId("media-hook-url").getAttribute("data-url"))
    .not.toBe(old);
  const urls = await page.evaluate(() => {
    const { state, probe } = (
      window as unknown as {
        __reloginMedia: {
          state: { revoked: string[]; created: string[] };
          probe: { dispose: () => void };
        };
      }
    ).__reloginMedia;
    probe.dispose();
    return state;
  });
  expect(urls.revoked.filter((url) => url === old)).toHaveLength(1);
  expect([...urls.revoked].sort()).toEqual([...urls.created].sort());
});
