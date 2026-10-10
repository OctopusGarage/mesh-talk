import { useEffect, useRef, useState } from "react";
import { Download, FolderOpen, Trash2 } from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useTranslation } from "react-i18next";
import { usePacks } from "@/store/packs";
import { useTheme } from "@/lib/theme";
import { MAX_PACK_ZIP_BYTES } from "@/lib/pack";

const MARKET_BASE = "https://octopusgarage.github.io/mesh-talk/market/";
interface CatalogEntry {
  id: string;
  name: string;
  version?: string;
  kind: "avatar" | "theme" | "sticker";
  category?: "personal" | "group";
  description: string;
  file: string;
  sha256: string;
}

function catalogEntries(value: unknown): CatalogEntry[] {
  if (!Array.isArray(value)) throw new Error("Invalid marketplace catalog");
  return value.map((item) => {
    if (!item || typeof item !== "object")
      throw new Error("Invalid marketplace entry");
    const entry = item as Record<string, unknown>;
    if (
      typeof entry.id !== "string" ||
      !/^[a-z0-9][a-z0-9._-]{2,79}$/.test(entry.id) ||
      typeof entry.name !== "string" ||
      (entry.version !== undefined &&
        (typeof entry.version !== "string" ||
          !/^\d+\.\d+\.\d+$/.test(entry.version))) ||
      typeof entry.description !== "string" ||
      (entry.kind !== "avatar" &&
        entry.kind !== "theme" &&
        entry.kind !== "sticker") ||
      (entry.kind === "avatar"
        ? entry.category !== "personal" && entry.category !== "group"
        : entry.category !== undefined) ||
      typeof entry.file !== "string" ||
      entry.file !== `packs/${entry.id}.zip` ||
      typeof entry.sha256 !== "string" ||
      !/^[a-f0-9]{64}$/.test(entry.sha256)
    ) {
      throw new Error("Invalid marketplace entry");
    }
    return entry as unknown as CatalogEntry;
  });
}

async function digest(bytes: Uint8Array): Promise<string> {
  const hash = await crypto.subtle.digest("SHA-256", new Uint8Array(bytes));
  return Array.from(new Uint8Array(hash), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

async function readDownload(response: Response): Promise<Uint8Array> {
  if (!response.body) throw new Error("Empty download");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    for (;;) {
      const result = await reader.read();
      if (result.done) break;
      size += result.value.byteLength;
      if (size > MAX_PACK_ZIP_BYTES) {
        await reader.cancel();
        throw new Error("Pack ZIP is too large");
      }
      chunks.push(result.value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

export function PackManager({
  kind,
  category,
}: {
  kind: "avatar" | "theme" | "sticker";
  category?: "personal" | "group";
}) {
  const { t } = useTranslation();
  const fileRef = useRef<HTMLInputElement>(null);
  const packs = usePacks((s) => s.packs).filter(
    (pack) =>
      pack.kind === kind &&
      (pack.kind !== "avatar" || pack.category === category),
  );
  const install = usePacks((s) => s.install);
  const remove = usePacks((s) => s.remove);
  const [catalog, setCatalog] = useState<CatalogEntry[]>([]);
  const [marketError, setMarketError] = useState(false);
  const [catalogRetry, setCatalogRetry] = useState(0);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    const controller = new AbortController();
    fetch(`${MARKET_BASE}catalog.json`, { signal: controller.signal })
      .then((response) => {
        if (!response.ok) throw new Error("Catalog unavailable");
        return response.json();
      })
      .then((value) => setCatalog(catalogEntries(value)))
      .catch(() => {
        if (!controller.signal.aborted) setMarketError(true);
      });
    return () => controller.abort();
  }, [catalogRetry]);

  const installBytes = async (bytes: Uint8Array, listing?: CatalogEntry) => {
    await install(bytes, (pack) => {
      if (
        listing &&
        (pack.id !== listing.id ||
          pack.name !== listing.name ||
          (listing.version !== undefined && pack.version !== listing.version) ||
          pack.kind !== listing.kind ||
          (pack.kind === "avatar" && pack.category !== listing.category))
      ) {
        throw new Error(t("packs.listingMismatch"));
      }
      if (
        pack.kind !== kind ||
        (pack.kind === "avatar" && pack.category !== category)
      ) {
        throw new Error(t("packs.wrongType"));
      }
    });
    useTheme.getState().refresh();
    setError("");
  };

  const localInstall = async (file?: File) => {
    if (!file) return;
    setBusy("local");
    try {
      if (file.size > MAX_PACK_ZIP_BYTES)
        throw new Error("Pack ZIP is too large");
      await installBytes(new Uint8Array(await file.arrayBuffer()));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  const marketInstall = async (item: CatalogEntry) => {
    setBusy(item.id);
    try {
      const response = await fetch(new URL(item.file, MARKET_BASE));
      if (!response.ok) throw new Error(t("packs.downloadFailed"));
      const bytes = await readDownload(response);
      if ((await digest(bytes)) !== item.sha256)
        throw new Error(t("packs.checksumFailed"));
      await installBytes(bytes, item);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const uninstall = async (id: string) => {
    setBusy(id);
    try {
      await remove(id);
      useTheme.getState().refresh();
      setError("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const available = catalog.filter(
    (item) =>
      item.kind === kind && (kind !== "avatar" || item.category === category),
  );
  return (
    <div className="space-y-3" data-testid={`pack-manager-${kind}`}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-xs font-medium text-muted-foreground">
          {t("packs.libraries")}
        </p>
        <>
          <input
            ref={fileRef}
            type="file"
            accept=".zip,application/zip"
            className="sr-only"
            aria-label={t("packs.importZip")}
            onChange={(event) => void localInstall(event.target.files?.[0])}
          />
          <button
            type="button"
            disabled={busy !== null}
            onClick={() => fileRef.current?.click()}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs font-medium hover:bg-accent disabled:opacity-50"
          >
            <FolderOpen className="h-3.5 w-3.5" />
            {t("packs.importZip")}
          </button>
        </>
      </div>
      {packs.length > 0 && (
        <div className="space-y-1" aria-label={t("packs.installed")}>
          {packs.map((pack) => (
            <div
              key={pack.id}
              className="flex items-center justify-between gap-2 rounded-md border border-border bg-muted/30 px-3 py-2 text-xs"
            >
              <span className="min-w-0 truncate">
                {pack.name}{" "}
                <span className="text-muted-foreground">v{pack.version}</span>
              </span>
              <button
                type="button"
                disabled={busy !== null}
                onClick={() => void uninstall(pack.id)}
                aria-label={`${t("packs.remove")} ${pack.name}`}
                className="shrink-0 rounded p-1 text-muted-foreground hover:bg-accent hover:text-destructive disabled:opacity-50"
              >
                <Trash2 className="h-3.5 w-3.5" />
              </button>
            </div>
          ))}
        </div>
      )}
      {available.length > 0 && (
        <div className="space-y-1" aria-label={t("packs.marketplace")}>
          <p className="text-xs font-medium text-muted-foreground">
            {t("packs.marketplace")}
          </p>
          {available.map((item) => {
            const installed = packs.find((pack) => pack.id === item.id);
            return (
              <div
                key={item.id}
                className="flex items-center justify-between gap-2 rounded-md border border-border px-3 py-2 text-xs"
              >
                <span className="min-w-0">
                  <strong className="block truncate font-medium">
                    {item.name}{" "}
                    {item.version && (
                      <span className="font-normal">v{item.version}</span>
                    )}
                  </strong>
                  <span className="line-clamp-2 text-muted-foreground">
                    {item.description}
                  </span>
                </span>
                <button
                  type="button"
                  disabled={busy !== null}
                  onClick={() => void marketInstall(item)}
                  className="inline-flex shrink-0 items-center gap-1 rounded-md bg-primary px-2 py-1.5 font-medium text-primary-foreground disabled:opacity-50"
                >
                  <Download className="h-3.5 w-3.5" />
                  {installed
                    ? item.version && installed.version === item.version
                      ? t("packs.reinstall")
                      : t("packs.replace")
                    : t("packs.install")}
                </button>
              </div>
            );
          })}
        </div>
      )}
      {marketError && (
        <div
          className="flex items-center gap-2 text-xs text-muted-foreground"
          role="status"
        >
          <span>{t("packs.marketUnavailable")}</span>
          <button
            type="button"
            onClick={() => {
              setMarketError(false);
              setCatalogRetry((count) => count + 1);
            }}
            className="shrink-0 text-primary underline"
          >
            {t("packs.retry")}
          </button>
        </div>
      )}
      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}
      <a
        href="https://octopusgarage.github.io/mesh-talk/market/"
        target="_blank"
        rel="noreferrer"
        onClick={(event) => {
          if (isTauri()) {
            event.preventDefault();
            void openUrl(event.currentTarget.href).catch((cause) =>
              setError(cause instanceof Error ? cause.message : String(cause)),
            );
          }
        }}
        className="inline-block text-xs text-primary underline"
      >
        {t("packs.browseMarket")}
      </a>
    </div>
  );
}
