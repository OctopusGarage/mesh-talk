import { beforeEach, expect, it, vi } from "vitest";
const m = vi.hoisted(() => ({
  favoriteAvailable: vi.fn(),
  saveFavorite: vi.fn(),
  save: vi.fn(),
  open: vi.fn(),
  saveFile: vi.fn(),
  saveFileToDir: vi.fn(),
  remember: vi.fn(),
  refresh: vi.fn(),
  ready: true,
  dir: "/configured",
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: m.save, open: m.open }));
vi.mock("@/lib/api/messageFavorites", () => ({
  messageFavorites: {
    fileAvailable: m.favoriteAvailable,
    saveFile: m.saveFavorite,
  },
}));
vi.mock("@/lib/api", () => ({
  chat: { saveFile: m.saveFile, saveFileToDir: m.saveFileToDir },
}));
vi.mock("./download", () => ({
  effectiveDownloadDir: async () => m.dir,
  defaultSavePath: async (name: string) => `${m.dir}/${name}`,
}));
vi.mock("./savedDownloads", () => ({ rememberSavedDownload: m.remember }));
vi.mock("@/store/fileAvailability", () => ({
  refreshFileAvailability: m.refresh,
  useFileAvailability: {
    getState: () => ({ statuses: { fc: { ready: m.ready } } }),
  },
}));
import { saveAttachment } from "./attachmentSave";
import { encodeStickerMediaName } from "./stickerMedia";
const file = { fileConv: "fc", name: "report.txt", mime: "text/plain" };
beforeEach(() => {
  vi.clearAllMocks();
  m.ready = true;
  m.favoriteAvailable.mockResolvedValue(true);
  m.saveFavorite.mockResolvedValue("/chosen/report.txt");
  m.dir = "/configured";
  m.refresh.mockResolvedValue(undefined);
  m.save.mockResolvedValue("/chosen/report.txt");
  m.saveFileToDir.mockResolvedValue("/configured/report (2).txt");
});
it("refreshes cached readiness after the dialog before exporting", async () => {
  m.refresh.mockImplementationOnce(async () => {
    m.ready = false;
  });
  await saveAttachment(file, { current: () => true }, { mode: "as" });
  expect(m.refresh).toHaveBeenCalledWith(["fc"]);
  expect(m.saveFile).not.toHaveBeenCalled();
});
it("passes the originating owner to native directory exports", async () => {
  await saveAttachment(
    file,
    { current: () => true, owner: "alice" },
    { mode: "default" },
  );
  expect(m.saveFileToDir).toHaveBeenCalledWith("fc", "/configured", "alice");
});
it("Save as prompts at the configured directory and remembers the selected path", async () => {
  await saveAttachment(file, { current: () => true }, { mode: "as" });
  expect(m.save).toHaveBeenCalledWith({
    defaultPath: "/configured/report.txt",
  });
  expect(m.saveFile).toHaveBeenCalledWith("fc", "/chosen/report.txt");
  expect(m.remember).toHaveBeenCalledWith("fc", "/chosen/report.txt");
});
it("suggests a safe readable sticker export name instead of its transport marker", async () => {
  const name = encodeStickerMediaName("calm", "🙂", "png", "静下心");
  await saveAttachment(
    { ...file, name, mime: "image/png", media: true, size: 42 },
    { current: () => true },
    { mode: "as" },
  );
  expect(m.save).toHaveBeenCalledWith({
    defaultPath: "/configured/静下心.png",
  });
});
it.each([
  ["../静/心", "_静_心.png"],
  ["CON", "_CON.png"],
  ["...", "sticker.png"],
])(
  "keeps sticker export label %s inside a legal filename",
  async (label, expected) => {
    const name = encodeStickerMediaName("calm", "🙂", "png", label);
    await saveAttachment(
      { ...file, name, mime: "image/png", media: true, size: 42 },
      { current: () => true },
      { mode: "as" },
    );
    expect(m.save).toHaveBeenCalledWith({
      defaultPath: `/configured/${expected}`,
    });
  },
);
it("Save default remembers the actual collision-resolved path", async () => {
  await saveAttachment(file, { current: () => true }, { mode: "default" });
  expect(m.save).not.toHaveBeenCalled();
  expect(m.saveFileToDir).toHaveBeenCalledWith("fc", "/configured", undefined);
  expect(m.remember).toHaveBeenCalledWith("fc", "/configured/report (2).txt");
});
it.each(["owner", "availability"])(
  "does not save after a pending dialog loses %s",
  async (lost) => {
    let current = true;
    m.save.mockImplementationOnce(async () => {
      if (lost === "owner") current = false;
      else m.ready = false;
      return "/chosen/report.txt";
    });
    await saveAttachment(file, { current: () => current }, { mode: "as" });
    expect(m.saveFile).not.toHaveBeenCalled();
    expect(m.remember).not.toHaveBeenCalled();
  },
);
it("does not remember a result after ownership changes during native save", async () => {
  let current = true;
  m.saveFile.mockImplementationOnce(async () => {
    current = false;
  });
  await saveAttachment(file, { current: () => current }, { mode: "as" });
  expect(m.remember).not.toHaveBeenCalled();
});
it("Save default falls back to a dialog when no directory resolves", async () => {
  m.dir = "";
  await saveAttachment(file, { current: () => true }, { mode: "default" });
  expect(m.save).toHaveBeenCalledOnce();
  expect(m.saveFileToDir).not.toHaveBeenCalled();
  expect(m.saveFile).toHaveBeenCalledWith("fc", "/chosen/report.txt");
});
it("canceling Save as leaves the attachment and remembered downloads unchanged", async () => {
  m.save.mockResolvedValueOnce(null);
  await saveAttachment(file, { current: () => true }, { mode: "as" });
  expect(m.saveFile).not.toHaveBeenCalled();
  expect(m.remember).not.toHaveBeenCalled();
});
it("a directory Save as prompts even with a configured default and remembers its actual path", async () => {
  m.open.mockResolvedValueOnce("/chosen");
  m.saveFileToDir.mockResolvedValueOnce("/chosen/folder (2)");
  await saveAttachment(
    { ...file, mime: "application/x-mesh-talk-directory-tar" },
    { current: () => true },
    { mode: "as" },
  );
  expect(m.open).toHaveBeenCalledWith({
    directory: true,
    defaultPath: "/configured/report.txt",
  });
  expect(m.saveFileToDir).toHaveBeenCalledWith("fc", "/chosen", undefined);
  expect(m.remember).toHaveBeenCalledWith("fc", "/chosen/folder (2)");
});

it("favorite exports use retained-file authority independently of ordinary history readiness", async () => {
  m.ready = false;
  await saveAttachment(
    file,
    { current: () => true, owner: "alice" },
    { mode: "as", source: "favorite" },
  );
  expect(m.favoriteAvailable).toHaveBeenCalledWith("alice", "fc");
  expect(m.saveFavorite).toHaveBeenCalledWith(
    "alice",
    "fc",
    "/chosen/report.txt",
    false,
  );
  expect(m.saveFile).not.toHaveBeenCalled();
});
it("favorite export rechecks retained reference after the dialog", async () => {
  m.favoriteAvailable.mockResolvedValueOnce(true).mockResolvedValueOnce(false);
  await saveAttachment(
    file,
    { current: () => true, owner: "alice" },
    { mode: "as", source: "favorite" },
  );
  expect(m.saveFavorite).not.toHaveBeenCalled();
});
