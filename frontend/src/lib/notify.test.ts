import { beforeEach, expect, it, vi } from "vitest";

const { check, request } = vi.hoisted(() => ({
  check: vi.fn(),
  request: vi.fn(),
}));
vi.mock("./api/notifications", () => ({
  notifications: { check, request },
}));
import { useNotificationPermission } from "./notify";
const allowed = {
  status: "granted",
  platform: "macos",
  can_open_settings: true,
};
const denied = { ...allowed, status: "denied" };
const prompt = { ...allowed, status: "prompt" };
beforeEach(() => {
  vi.resetAllMocks();
  useNotificationPermission.setState({
    permission: null,
    checking: false,
    requesting: false,
  });
});
it("checks again after the OS permission has been revoked, without requesting", async () => {
  check.mockResolvedValueOnce(allowed).mockResolvedValueOnce(denied);
  await useNotificationPermission.getState().refresh();
  expect(useNotificationPermission.getState().permission).toEqual(allowed);
  await useNotificationPermission.getState().refresh();
  expect(useNotificationPermission.getState().permission).toEqual(denied);
  expect(request).not.toHaveBeenCalled();
});
it("shares concurrent observations and never prompts during a read", async () => {
  let resolve!: (value: unknown) => void;
  check.mockReturnValue(
    new Promise((done) => {
      resolve = done;
    }),
  );
  const first = useNotificationPermission.getState().refresh();
  const second = useNotificationPermission.getState().refresh();
  expect(check).toHaveBeenCalledOnce();
  resolve(prompt);
  await Promise.all([first, second]);
  expect(request).not.toHaveBeenCalled();
  expect(useNotificationPermission.getState().checking).toBe(false);
});
it("requests only on an explicit action and only for undetermined permission", async () => {
  check.mockResolvedValue(prompt);
  request.mockResolvedValue(allowed);
  await Promise.all([
    useNotificationPermission.getState().enable(),
    useNotificationPermission.getState().enable(),
  ]);
  expect(request).toHaveBeenCalledOnce();
  expect(useNotificationPermission.getState().permission).toEqual(allowed);
  check.mockResolvedValue(denied);
  await useNotificationPermission.getState().enable();
  expect(request).toHaveBeenCalledOnce();
  expect(useNotificationPermission.getState().permission).toEqual(denied);
});
it("does not let a focus read overwrite the result of an explicit request", async () => {
  let resolve!: (value: unknown) => void;
  check.mockResolvedValue(prompt);
  request.mockReturnValue(
    new Promise((done) => {
      resolve = done;
    }),
  );
  const enabling = useNotificationPermission.getState().enable();
  await vi.waitFor(() => expect(request).toHaveBeenCalledOnce());
  const refreshing = useNotificationPermission.getState().refresh();
  resolve(allowed);
  await Promise.all([enabling, refreshing]);
  expect(useNotificationPermission.getState().permission).toEqual(allowed);
  expect(check).toHaveBeenCalledOnce();
});
it("reports a failed observation as unknown instead of retaining a cached grant", async () => {
  check
    .mockResolvedValueOnce(allowed)
    .mockRejectedValueOnce(new Error("unavailable"));
  await useNotificationPermission.getState().refresh();
  await useNotificationPermission.getState().refresh();
  expect(useNotificationPermission.getState().permission).toEqual({
    ...allowed,
    status: "unknown",
  });
  expect(useNotificationPermission.getState().checking).toBe(false);
});
it("settles a failed explicit request without retrying or claiming a grant", async () => {
  check.mockResolvedValue(prompt);
  request.mockRejectedValue(new Error("unavailable"));
  await useNotificationPermission.getState().enable();
  expect(useNotificationPermission.getState().permission?.status).toBe(
    "unknown",
  );
  expect(useNotificationPermission.getState().requesting).toBe(false);
  expect(request).toHaveBeenCalledOnce();
});

it("does not open a delayed prompt after the originating view has closed", async () => {
  let resolve!: (value: unknown) => void;
  let current = true;
  check.mockReturnValue(
    new Promise((done) => {
      resolve = done;
    }),
  );
  const enabling = useNotificationPermission.getState().enable(() => current);
  current = false;
  resolve(prompt);
  await enabling;
  expect(request).not.toHaveBeenCalled();
});
