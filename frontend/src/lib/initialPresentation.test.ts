import { expect, it, vi } from "vitest";
import { prepareInitialPresentation } from "./initialPresentation";

it("waits for wallpaper decoding and a hidden-safe layout turn before acknowledging", async () => {
  let release!: () => void;
  const decode = vi.fn(
    () =>
      new Promise<void>((resolve) => {
        release = resolve;
      }),
  );
  const commit = vi.fn(async () => {});
  const acknowledge = vi.fn(async () => {});
  const pending = prepareInitialPresentation("fixture:image", () => true, {
    decode,
    commit,
    acknowledge,
  });
  expect(acknowledge).not.toHaveBeenCalled();
  release();
  await pending;
  expect(decode).toHaveBeenCalledWith("fixture:image");
  expect(commit).toHaveBeenCalledOnce();
  expect(acknowledge).toHaveBeenCalledOnce();
});

it("never acknowledges a retired account after an image finishes decoding", async () => {
  let current = true;
  let release!: () => void;
  const acknowledge = vi.fn(async () => {});
  const pending = prepareInitialPresentation("fixture:image", () => current, {
    decode: () =>
      new Promise<void>((resolve) => {
        release = resolve;
      }),
    commit: async () => {},
    acknowledge,
  });
  current = false;
  release();
  await pending;
  expect(acknowledge).not.toHaveBeenCalled();
});

it("settles safe login and locked surfaces without decoding private images", async () => {
  const decode = vi.fn(async () => {});
  const acknowledge = vi.fn(async () => {});
  await prepareInitialPresentation(null, () => true, {
    decode,
    commit: async () => {},
    acknowledge,
  });
  expect(decode).not.toHaveBeenCalled();
  expect(acknowledge).toHaveBeenCalledOnce();
});
