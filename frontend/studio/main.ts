import { strToU8, zipSync } from "fflate";
import {
  MAX_PACK_ZIP_BYTES,
  parsePack,
  referencedPackImages,
  type CustomizationPack,
} from "../src/lib/pack";
import { verifyPackImages } from "../src/lib/packImages";
import cliSource from "../scripts/pack.mjs?raw";
import parserSource from "../src/lib/pack.ts?raw";
import "./style.css";

function byId<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing page element: ${id}`);
  return element as T;
}

const zipInput = byId<HTMLInputElement>("zip-input");
const folderInput = byId<HTMLInputElement>("folder-input");
const dropZone = byId<HTMLButtonElement>("drop-zone");
const result = byId<HTMLElement>("result");
const resultBadge = byId<HTMLElement>("result-badge");
const resultTitle = byId<HTMLElement>("result-title");
const resultMessage = byId<HTMLElement>("result-message");
const resultFacts = byId<HTMLElement>("result-facts");
const preview = byId<HTMLElement>("preview");
const toolkitStatus = byId<HTMLElement>("toolkit-status");
let currentOperation = 0;

function begin(action: string, label: string): number {
  const operation = ++currentOperation;
  result.hidden = false;
  result.classList.remove("error");
  result.setAttribute("aria-busy", "true");
  resultBadge.textContent = "Checking";
  resultTitle.textContent = label;
  resultMessage.textContent = action;
  resultFacts.replaceChildren();
  preview.replaceChildren();
  return operation;
}

function fact(label: string, value: string): void {
  const group = document.createElement("div");
  const term = document.createElement("dt");
  term.textContent = label;
  const description = document.createElement("dd");
  description.textContent = value;
  group.append(term, description);
  resultFacts.append(group);
}

function packImages(pack: CustomizationPack): { url: string; label: string }[] {
  if (pack.kind === "avatar")
    return pack.avatars.map(({ url, label }) => ({ url, label }));
  if (pack.kind === "sticker")
    return pack.stickers.map(({ url, label }) => ({ url, label }));
  return [
    ...(pack.wallpaper ? [{ url: pack.wallpaper, label: "Wallpaper" }] : []),
    ...(pack.crest ? [{ url: pack.crest, label: "Crest" }] : []),
    ...(pack.wallpapers ?? []).map(({ url, title }) => ({ url, label: title })),
  ];
}

function showError(action: string, error: unknown, operation: number): void {
  if (operation !== currentOperation) return;
  result.hidden = false;
  result.classList.add("error");
  result.removeAttribute("aria-busy");
  resultBadge.textContent = "Failed";
  resultTitle.textContent = `Could not ${action} this pack`;
  resultMessage.textContent =
    error instanceof Error ? error.message : String(error);
  resultFacts.replaceChildren();
  preview.replaceChildren();
  result.scrollIntoView({ block: "nearest" });
}

async function sha256(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new Uint8Array(bytes));
  return Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

async function showVerified(
  pack: CustomizationPack,
  bytes: Uint8Array,
  built: boolean,
  operation: number,
  ignoredFiles = 0,
): Promise<void> {
  const checksum = await sha256(bytes);
  if (operation !== currentOperation) return;
  const images = packImages(pack);
  result.hidden = false;
  result.classList.remove("error");
  result.removeAttribute("aria-busy");
  resultBadge.textContent = "Passed";
  resultTitle.textContent = pack.name;
  resultMessage.textContent = built
    ? `Your ZIP was built and checked. Download starting.${ignoredFiles ? ` ${ignoredFiles} unrelated source ${ignoredFiles === 1 ? "file was" : "files were"} left out.` : ""}`
    : "Technical checks passed. Review the artwork and its rights before sharing.";
  resultFacts.replaceChildren();
  fact("Type", pack.kind === "avatar" ? `${pack.category} avatars` : pack.kind);
  fact("Version / images", `${pack.version} / ${images.length}`);
  fact("SHA-256", checksum);
  preview.replaceChildren();
  for (const image of images.slice(0, 8)) {
    const thumbnail = document.createElement("img");
    thumbnail.src = image.url;
    thumbnail.alt = image.label;
    thumbnail.loading = "lazy";
    preview.append(thumbnail);
  }
  result.scrollIntoView({ block: "nearest" });
}

async function checkBytes(
  bytes: Uint8Array,
  operation: number,
  built = false,
  ignoredFiles = 0,
): Promise<CustomizationPack> {
  const pack = parsePack(bytes);
  await verifyPackImages(pack);
  await showVerified(pack, bytes, built, operation, ignoredFiles);
  return pack;
}

async function verifyFile(file: File): Promise<void> {
  const operation = begin(
    "Checking the ZIP and decoding its images…",
    file.name,
  );
  try {
    if (file.size > MAX_PACK_ZIP_BYTES)
      throw new Error("Pack ZIP is too large (12 MiB maximum)");
    await checkBytes(new Uint8Array(await file.arrayBuffer()), operation);
  } catch (error) {
    showError("verify", error, operation);
  }
}

function download(bytes: Uint8Array, name: string): void {
  const url = URL.createObjectURL(
    new Blob([new Uint8Array(bytes)], { type: "application/zip" }),
  );
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

async function buildFolder(selected: FileList): Promise<void> {
  const operation = begin("Building and checking your ZIP…", "Source folder");
  try {
    const files = new Map<string, File>();
    for (const file of selected) {
      const parts = file.webkitRelativePath.split("/");
      const name = parts.length > 1 ? parts.slice(1).join("/") : file.name;
      if (files.has(name)) throw new Error(`Duplicate source file: ${name}`);
      files.set(name, file);
    }
    const manifestFile = files.get("manifest.json");
    if (!manifestFile)
      throw new Error("Choose a folder with manifest.json at its root");
    if (manifestFile.size > 64 * 1024)
      throw new Error("manifest.json is too large (64 KiB maximum)");
    const manifestBytes = new Uint8Array(await manifestFile.arrayBuffer());
    const manifest = JSON.parse(new TextDecoder().decode(manifestBytes));
    const archive: Record<string, Uint8Array> = {
      "manifest.json": manifestBytes,
    };
    let unpackedBytes = manifestFile.size;
    const referenced = referencedPackImages(manifest);
    for (const name of referenced) {
      const image = files.get(name);
      if (!image) throw new Error(`Missing image: ${name}`);
      if (image.size > 3 * 1024 * 1024)
        throw new Error(`Image is too large: ${name}`);
      unpackedBytes += image.size;
      if (unpackedBytes > 24 * 1024 * 1024)
        throw new Error("Pack content is too large (24 MiB maximum)");
      archive[name] = new Uint8Array(await image.arrayBuffer());
    }
    const credits = files.get("credits.json");
    if (credits) {
      if (credits.size > 64 * 1024)
        throw new Error("credits.json is too large (64 KiB maximum)");
      archive["credits.json"] = new Uint8Array(await credits.arrayBuffer());
    }
    const included = new Set(["manifest.json", "credits.json", ...referenced]);
    const ignoredFiles = [...files.keys()].filter(
      (name) => !included.has(name),
    ).length;
    const bytes = zipSync(archive, { level: 6 });
    const pack = await checkBytes(bytes, operation, true, ignoredFiles);
    if (operation === currentOperation) download(bytes, `${pack.id}.zip`);
  } catch (error) {
    showError("build", error, operation);
  }
}

async function samplePng(): Promise<Uint8Array> {
  const canvas = document.createElement("canvas");
  canvas.width = 160;
  canvas.height = 160;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("Canvas is unavailable");
  context.fillStyle = "#102832";
  context.fillRect(0, 0, 160, 160);
  context.fillStyle = "#2fe6ce";
  context.beginPath();
  context.arc(80, 80, 57, 0, Math.PI * 2);
  context.fill();
  context.fillStyle = "#08141a";
  context.font = "bold 49px system-ui";
  context.textAlign = "center";
  context.textBaseline = "middle";
  context.fillText("MT", 80, 81);
  const blob = await new Promise<Blob>((resolve, reject) =>
    canvas.toBlob(
      (value) =>
        value
          ? resolve(value)
          : reject(new Error("Could not create sample image")),
      "image/png",
    ),
  );
  return new Uint8Array(await blob.arrayBuffer());
}

async function downloadKit(): Promise<void> {
  const image = await samplePng();
  const manifests = {
    avatar: {
      format: 1,
      id: "example.avatars",
      version: "1.0.0",
      name: "My Avatars",
      kind: "avatar",
      category: "personal",
      fit: "cover",
      avatars: [{ label: "Sample", file: "images/sample.png" }],
    },
    theme: {
      format: 1,
      id: "example.theme",
      version: "1.0.0",
      name: "My Theme",
      kind: "theme",
      base: "dark",
      colors: {
        background: "210 25% 12%",
        foreground: "160 20% 94%",
        primary: "170 70% 55%",
      },
      wallpaper: "images/sample.png",
    },
    sticker: {
      format: 1,
      id: "example.stickers",
      version: "1.0.0",
      name: "My Stickers",
      kind: "sticker",
      stickers: [
        {
          id: "sample",
          label: "Sample",
          fallback: "✨",
          file: "images/sample.png",
        },
      ],
    },
  };
  const kit: Record<string, Uint8Array> = {
    "README.md": strToU8(
      "# Mesh-Talk creator toolkit\n\nEach examples/ subfolder is one independent pack source folder. Replace sample.png with your artwork and edit manifest.json.\n\nRequires Node.js 22.6+. Run `npm install`, then `npm run pack:build -- examples/avatar avatar.zip` and `npm run pack:check -- avatar.zip`.\n\nThe checker validates structure and signatures; the app and web Studio also decode images. None of these checks establish image rights or determine whether artwork is appropriate.\n\nFull guide: https://github.com/OctopusGarage/mesh-talk/blob/main/docs/CUSTOMIZATION_PACKS.md\n",
    ),
    "package.json": strToU8(
      JSON.stringify(
        {
          private: true,
          type: "module",
          scripts: {
            "pack:build":
              "node --experimental-strip-types scripts/pack.mjs build",
            "pack:check":
              "node --experimental-strip-types scripts/pack.mjs check",
          },
          dependencies: { fflate: "0.8.3" },
        },
        null,
        2,
      ) + "\n",
    ),
    "scripts/pack.mjs": strToU8(cliSource),
    "src/lib/pack.ts": strToU8(parserSource),
  };
  for (const [kind, manifest] of Object.entries(manifests)) {
    kit[`examples/${kind}/manifest.json`] = strToU8(
      JSON.stringify(manifest, null, 2) + "\n",
    );
    kit[`examples/${kind}/images/sample.png`] = image;
  }
  download(zipSync(kit, { level: 6 }), "mesh-talk-pack-toolkit.zip");
}

zipInput.addEventListener("change", () => {
  const file = zipInput.files?.[0];
  if (file) void verifyFile(file);
  zipInput.value = "";
});
dropZone.addEventListener("click", () => zipInput.click());
byId<HTMLButtonElement>("folder-button").addEventListener("click", () =>
  folderInput.click(),
);
folderInput.addEventListener("change", () => {
  if (folderInput.files?.length) void buildFolder(folderInput.files);
  folderInput.value = "";
});
dropZone.addEventListener("dragover", (event) => {
  event.preventDefault();
  dropZone.classList.add("drag-over");
});
dropZone.addEventListener("dragleave", () =>
  dropZone.classList.remove("drag-over"),
);
dropZone.addEventListener("drop", (event) => {
  event.preventDefault();
  dropZone.classList.remove("drag-over");
  const file = event.dataTransfer?.files[0];
  if (file) void verifyFile(file);
});
byId<HTMLButtonElement>("download-kit").addEventListener("click", () => {
  toolkitStatus.textContent = "Preparing creator toolkit…";
  void downloadKit()
    .then(() => {
      toolkitStatus.textContent = "Creator toolkit downloaded.";
    })
    .catch((error) => {
      toolkitStatus.textContent = `Could not download toolkit: ${error instanceof Error ? error.message : String(error)}`;
    });
});
