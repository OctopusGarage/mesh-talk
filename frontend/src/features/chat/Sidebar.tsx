import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  LogOut,
  MoreHorizontal,
  Moon,
  Network,
  Wifi,
  WifiOff,
  Pencil,
  Pin,
  PinOff,
  Sun,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { IdentityGlyph, PresenceDot } from "@/components/identity";
import { Logo } from "@/components/Logo";
import { cn } from "@/lib/utils";
import { shortId } from "@/lib/format";
import { useTheme } from "@/lib/theme";
import { THEME_CREST } from "@/lib/themeCrest";
import { CreateChannelDialog } from "./CreateChannelDialog";
import { SearchDialog } from "./SearchDialog";
import { FilesTray } from "./FilesTray";
import { LinkDeviceDialog } from "./LinkDeviceDialog";
import { DiagnosticsDialog } from "./DiagnosticsDialog";
import { OfflineConnectDialog } from "./OfflineConnectDialog";
import { SettingsDialog } from "./SettingsDialog";
import { ProfileDialog } from "./ProfileDialog";
import { AboutDialog } from "./AboutDialog";
import { useAuth } from "@/store/auth";
import { chat, diag } from "@/lib/api";
import { GroupAvatar } from "@/components/GroupAvatar";
import { convKey, useChat, type Conversation } from "@/store/chat";
import {
  presenceLabel,
  presenceStatus,
  usePresence,
  usePresenceFor,
} from "@/store/presence";
import type { AccountInfo, ChannelInfo } from "@/lib/types";

function accountConv(a: AccountInfo, name: string): Conversation {
  return {
    kind: "account",
    id: a.account_id,
    name,
  };
}
function channelConv(c: ChannelInfo, name: string): Conversation {
  return { kind: "channel", id: c.channel_id, name };
}

function Row({
  conv,
  subtitle,
  channel,
  pinned,
  onTogglePin,
  onRename,
}: {
  conv: Conversation;
  subtitle: string;
  channel?: boolean;
  pinned: boolean;
  onTogglePin: () => void;
  onRename: () => void;
}) {
  const { t } = useTranslation();
  const active = useChat((s) => s.active);
  const unread = useChat((s) => s.unread[convKey(conv)] ?? 0);
  const open = useChat((s) => s.open);
  const isActive = active != null && convKey(active) === convKey(conv);
  // Presence is read from the isolated store and keyed by id, so a presence tick only
  // re-renders the rows whose snapshot actually changed.
  const presence = usePresenceFor(conv.id);
  const status = presenceStatus(presence);

  return (
    <div
      role="listitem"
      className={cn(
        "group relative flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left transition-colors duration-150 ease-out",
        isActive ? "bg-accent" : "hover:bg-accent/60",
      )}
    >
      {/* Teal active rail. */}
      {isActive && (
        <span
          aria-hidden
          className="absolute inset-y-1.5 left-0 w-0.5 rounded-full bg-signal"
        />
      )}
      <button
        onClick={() => open(conv)}
        data-conv-option
        data-testid={`conversation-row-${conv.id}`}
        aria-current={isActive ? "true" : undefined}
        aria-label={`${conv.name}${subtitle ? `, ${subtitle}` : ""}`}
        className="flex min-w-0 flex-1 items-center gap-3 rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <div className="relative">
          {channel ? (
            <GroupAvatar channelId={conv.id} size={36} title={conv.name} />
          ) : (
            <IdentityGlyph seed={conv.id} size={36} title={conv.name} />
          )}
          {/* Presence overlays the glyph — the signature living-LAN cue. */}
          {!channel && (
            <PresenceDot
              status={status}
              size="md"
              label={presenceLabel(presence, t)}
              className="absolute -bottom-0.5 -right-0.5"
            />
          )}
        </div>
        <div className="min-w-0 flex-1">
          <div className="truncate font-display text-sm font-medium tracking-tight">
            {conv.name}
          </div>
          <div className="truncate font-mono text-xs text-muted-foreground">
            {subtitle}
          </div>
        </div>
      </button>
      {unread > 0 && (
        <Badge className="bg-signal font-mono text-[11px] text-primary-foreground">
          {unread}
        </Badge>
      )}
      <button
        type="button"
        onClick={onRename}
        title={t("sidebar.rename")}
        aria-label={t("sidebar.rename")}
        className="hidden rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground group-hover:block"
      >
        <Pencil className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={onTogglePin}
        data-testid={`conversation-pin-${conv.id}`}
        title={pinned ? t("sidebar.unpin") : t("sidebar.pin")}
        aria-label={pinned ? t("sidebar.unpin") : t("sidebar.pin")}
        className={cn(
          "rounded-md p-1 transition-colors hover:bg-accent",
          pinned
            ? "text-signal"
            : "hidden text-muted-foreground hover:text-foreground group-hover:block",
        )}
      >
        {pinned ? (
          <PinOff className="h-3.5 w-3.5" />
        ) : (
          <Pin className="h-3.5 w-3.5" />
        )}
      </button>
    </div>
  );
}

function SectionLabel({
  children,
  action,
}: {
  children: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between px-2.5 pb-1 pt-4 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
      <span>{children}</span>
      {action}
    </div>
  );
}

// Sidebar width: user-resizable via the right-edge drag handle, persisted to localStorage,
// clamped to a sensible range, double-click to reset. Default mirrors the old `w-72`.
const WIDTH_KEY = "mesh-talk-sidebar-width";
const DEFAULT_WIDTH = 288; // w-72
const MIN_WIDTH = 230;
const MAX_WIDTH = 460;

const clampWidth = (w: number) =>
  Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(w)));

function readWidth(): number {
  if (typeof localStorage === "undefined") return DEFAULT_WIDTH;
  const v = Number(localStorage.getItem(WIDTH_KEY));
  return Number.isFinite(v) && v > 0 ? clampWidth(v) : DEFAULT_WIDTH;
}

function useSidebarWidth() {
  const [width, setWidth] = useState(readWidth);
  const dragging = useRef(false);

  const persist = useCallback((w: number) => {
    setWidth(w);
    if (typeof localStorage !== "undefined")
      localStorage.setItem(WIDTH_KEY, String(w));
  }, []);

  // Drag-to-resize: track pointer moves on the document so the cursor can leave the thin
  // handle without dropping the drag. Width = pointer x relative to the sidebar's left edge.
  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      e.preventDefault();
      dragging.current = true;
      const aside = (e.currentTarget as HTMLElement).parentElement;
      const left = aside?.getBoundingClientRect().left ?? 0;
      const prevCursor = document.body.style.cursor;
      const prevSelect = document.body.style.userSelect;
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      const onMove = (ev: PointerEvent) => {
        if (!dragging.current) return;
        persist(clampWidth(ev.clientX - left));
      };
      const onUp = () => {
        dragging.current = false;
        document.body.style.cursor = prevCursor;
        document.body.style.userSelect = prevSelect;
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
    },
    [persist],
  );

  const reset = useCallback(() => persist(DEFAULT_WIDTH), [persist]);

  // Keyboard resize for accessibility (focus the handle, arrow to nudge).
  const onKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "ArrowLeft") persist(clampWidth(width - 16));
      else if (e.key === "ArrowRight") persist(clampWidth(width + 16));
      else return;
      e.preventDefault();
    },
    [persist, width],
  );

  return { width, onPointerDown, reset, onKeyDown };
}

/** A tidy popover collecting the secondary/utility actions (files, link-device,
 *  diagnostics, settings, about, theme toggle, sign-out). Keeps the top clean; the
 *  primary Search stays up top. Each action keeps its existing `data-testid`. */
function UtilityMenu() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const theme = useTheme((s) => s.theme);
  const toggleTheme = useTheme((s) => s.toggle);
  const logout = useAuth((s) => s.logout);

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          data-testid="sidebar-overflow"
          title={t("sidebar.moreActions")}
          aria-label={t("sidebar.moreActions")}
        >
          <MoreHorizontal className="h-4 w-4" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        side="top"
        className="w-auto p-1.5"
        data-testid="sidebar-overflow-menu"
      >
        {/* The dialog/popover trigger buttons keep their own testids + render their own
            icon and open their dialog; collected here as a quiet utility toolbar. */}
        <div className="grid gap-0.5">
          <div className="flex items-center gap-0.5">
            <LinkDeviceDialog />
            <DiagnosticsDialog />
            <SettingsDialog />
            <AboutDialog />
          </div>
          <div className="my-1 h-px bg-border" />
          <button
            type="button"
            data-testid="sidebar-theme-toggle"
            onClick={() => toggleTheme()}
            className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-accent"
          >
            {theme === "light" ? (
              <Moon className="h-4 w-4 text-muted-foreground" />
            ) : (
              <Sun className="h-4 w-4 text-muted-foreground" />
            )}
            <span>
              {theme === "light"
                ? t("sidebar.darkMode")
                : t("sidebar.lightMode")}
            </span>
          </button>
          <button
            type="button"
            data-testid="sidebar-sign-out"
            onClick={() => logout()}
            className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-sm text-destructive transition-colors hover:bg-destructive/10"
          >
            <LogOut className="h-4 w-4" />
            <span>{t("sidebar.signOut")}</span>
          </button>
        </div>
      </PopoverContent>
    </Popover>
  );
}

export function Sidebar() {
  const { t } = useTranslation();
  const accounts = useChat((s) => s.accounts);
  const channels = useChat((s) => s.channels);
  const peers = useChat((s) => s.peers);
  const favorites = useChat((s) => s.favorites);
  // The active theme's crest (a brand theme) replaces the app mark in the footer, so the
  // chosen club/national/Messi identity is always present without shouting.
  const themeCrest = THEME_CREST[useTheme((s) => s.theme)];
  const togglePinned = useChat((s) => s.togglePinned);
  const setAlias = useChat((s) => s.setAlias);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const ready = useChat((s) => s.ready);
  // Online people on the LAN — distinct OTHER accounts that are presence-ONLINE (the same
  // signal the conversation-row dots use, so the count always matches the visible green dots).
  // Excludes post-office relays (infrastructure, not people) and our own other devices, and
  // ignores roster entries that are merely lingering (seen, but past the online window) so the
  // number doesn't drift from what's actually online.
  const presenceMap = usePresence((s) => s.map);
  const onlinePeople = useMemo(() => {
    const online = new Set<string>();
    for (const p of peers) {
      if (p.post_office) continue;
      const account = p.account_id ?? p.user_id;
      if (account === myAccountId) continue;
      if (presenceMap[account]?.online) online.add(account);
    }
    return online.size;
  }, [peers, presenceMap, myAccountId]);
  const bootFailed = useChat((s) => s.bootFailed);
  const retryBoot = useChat((s) => s.retryBoot);
  const username = useAuth((s) => s.user?.username ?? "");
  // The peer-facing display name (nickname) shown in the identity header; falls back to
  // the login username. `username` is kept as the stable last-resort id/avatar seed.
  const displayName = useAuth(
    (s) => s.user?.display_name || s.user?.username || "",
  );
  const { width, onPointerDown, reset, onKeyDown } = useSidebarWidth();

  // Inline rename dialog state (a contact id + a draft alias). Kept local/primitive.
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  // The own-profile dialog (avatar + display name editing), opened from the identity header.
  const [profileOpen, setProfileOpen] = useState(false);
  // The Wi-Fi network (SSID) we're on — Mesh-Talk is LAN-scoped, so show which network the
  // peers around us share. Polled (it can change when you switch networks); null when wired
  // or the OS withholds it.
  const [ssid, setSsid] = useState<string | null>(null);
  // Whether this device has no usable (non-loopback) network interface at all — the strongest
  // "you're stranded" signal, which sharpens the offline-connect prompt's wording.
  const [noNetwork, setNoNetwork] = useState(false);
  useEffect(() => {
    let alive = true;
    const refresh = () => {
      chat
        .networkName()
        .then((n) => alive && setSsid(n))
        .catch(() => {});
      diag
        .networkInfo()
        .then((i) => alive && setNoNetwork(i.interfaces.length === 0))
        .catch(() => {});
    };
    refresh();
    const id = setInterval(refresh, 60_000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);

  // "Offline direct connect" prompt — surfaces only when stranded: the node is up but, after a
  // grace period (discovery's startup burst gets a fair chance), still nobody is online around
  // us. Dismissible per session; also always reachable from the diagnostics Troubleshoot tab.
  const [offlineOpen, setOfflineOpen] = useState(false);
  const [strandedDismissed, setStrandedDismissed] = useState(false);
  const [graceElapsed, setGraceElapsed] = useState(false);
  useEffect(() => {
    if (!ready) return;
    const id = setTimeout(() => setGraceElapsed(true), 20_000);
    return () => clearTimeout(id);
  }, [ready]);
  const stranded =
    ready && graceElapsed && onlinePeople === 0 && !strandedDismissed;

  // Roving keyboard navigation across the conversation rows. Arrow Up/Down moves focus
  // between the option buttons within the list (Enter/Space already open via the native
  // button). Scoped to the nav so it never traps the composer or other controls.
  const navRef = useRef<HTMLElement>(null);
  const onNavKeyDown = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const nav = navRef.current;
    if (!nav) return;
    const options = Array.from(
      nav.querySelectorAll<HTMLButtonElement>("[data-conv-option]"),
    );
    if (options.length === 0) return;
    const idx = options.indexOf(document.activeElement as HTMLButtonElement);
    e.preventDefault();
    const next =
      e.key === "ArrowDown"
        ? idx < 0
          ? 0
          : Math.min(idx + 1, options.length - 1)
        : idx <= 0
          ? 0
          : idx - 1;
    options[next]?.focus();
  };

  const startRename = (id: string, current: string) => {
    setRenameId(id);
    setRenameDraft(favorites[id]?.custom_alias ?? current);
  };
  const commitRename = () => {
    if (renameId) void setAlias(renameId, renameDraft);
    setRenameId(null);
  };

  // Resolve the displayed name (alias overrides the announced name) and split into
  // pinned vs the rest. Sort is stable on the source order within each group. Memoized so
  // the map + four partition passes don't re-run on every render (the sidebar re-renders
  // on the 4s roster refresh and on every favorites change).
  const {
    pinnedAccounts,
    unpinnedAccounts,
    pinnedChannels,
    unpinnedChannels,
    hasPinned,
  } = useMemo(() => {
    const accountRows = accounts.map((a) => {
      const id = a.account_id;
      const announced = a.names[0] || shortId(id);
      const name = favorites[id]?.custom_alias || announced;
      return {
        a,
        id,
        conv: accountConv(a, name),
        pinned: favorites[id]?.pinned ?? false,
        subtitle:
          a.device_count > 1
            ? t("sidebar.devices", { count: a.device_count })
            : shortId(id, 12),
      };
    });
    const channelRows = channels.map((c) => {
      const id = c.channel_id;
      const name = favorites[id]?.custom_alias || c.name;
      return {
        c,
        id,
        conv: channelConv(c, name),
        pinned: favorites[id]?.pinned ?? false,
        subtitle: shortId(id, 12),
      };
    });

    const pinnedAccounts = accountRows.filter((r) => r.pinned);
    const unpinnedAccounts = accountRows.filter((r) => !r.pinned);
    const pinnedChannels = channelRows.filter((r) => r.pinned);
    const unpinnedChannels = channelRows.filter((r) => !r.pinned);
    return {
      pinnedAccounts,
      unpinnedAccounts,
      pinnedChannels,
      unpinnedChannels,
      hasPinned: pinnedAccounts.length + pinnedChannels.length > 0,
    };
  }, [accounts, channels, favorites, t]);

  return (
    <aside
      data-testid="sidebar"
      style={{ width }}
      className="relative flex shrink-0 flex-col border-r bg-card/40"
    >
      {/* Identity header — own glyph + name (display) + own short mono id, plus the one
          primary action (Search). `data-tauri-drag-region` makes the strip a window-drag
          handle (interactive controls inside opt out via their own pointer handling);
          `data-titlebar-inset` clears the macOS traffic-lights (no-op elsewhere). */}
      <div
        className="border-b px-4 py-3"
        data-testid="self-identity"
        data-tauri-drag-region
        data-titlebar-inset="left"
      >
        <div className="flex items-center gap-3">
          {/* Avatar opens the profile (where the photo + name are edited) — like tapping
              your avatar in WeChat/Telegram. */}
          <button
            type="button"
            data-testid="open-profile"
            onClick={() => setProfileOpen(true)}
            aria-label={t("profile.open")}
            className="relative shrink-0 rounded-[28%] outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <IdentityGlyph
              seed={myAccountId || myId || username}
              size={38}
              title={displayName}
            />
            <PresenceDot
              status={ready ? "online" : "offline"}
              size="md"
              label={ready ? t("presence.online") : t("common.starting")}
              className="pointer-events-none absolute -bottom-0.5 -right-0.5"
            />
          </button>
          <div className="min-w-0 flex-1">
            {/* The name also opens the profile. Kept separate from the status line below so
                the boot-retry button never nests inside another button. */}
            <button
              type="button"
              onClick={() => setProfileOpen(true)}
              className="block max-w-full rounded outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <span
                data-testid="sidebar-own-name"
                className="block truncate text-left font-display text-sm font-semibold tracking-tight"
              >
                {displayName}
              </span>
            </button>
            <div className="truncate font-mono text-xs text-muted-foreground">
              {ready ? (
                t("sidebar.you", { id: shortId(myId) })
              ) : bootFailed ? (
                <button
                  type="button"
                  onClick={() => retryBoot()}
                  className="text-destructive underline-offset-2 hover:underline"
                >
                  {t("sidebar.nodeUnavailable")}
                </button>
              ) : (
                t("common.starting")
              )}
            </div>
          </div>
        </div>
        {/* Primary actions up top: Search + Received files. Other utilities live in the
            bottom-left overflow menu. */}
        <div className="mt-2.5 flex items-center gap-0.5">
          <SearchDialog />
          <FilesTray />
        </div>
        <ProfileDialog open={profileOpen} onOpenChange={setProfileOpen} />
      </div>

      <nav
        ref={navRef}
        role="list"
        aria-label={t("conversation.list")}
        onKeyDown={onNavKeyDown}
        className="flex-1 overflow-y-auto px-2 pb-4"
      >
        {hasPinned && (
          <>
            <SectionLabel>{t("sidebar.pinned")}</SectionLabel>
            {pinnedAccounts.map((r) => (
              <Row
                key={r.id}
                conv={r.conv}
                subtitle={r.subtitle}
                pinned
                onTogglePin={() => void togglePinned(r.id, false)}
                onRename={() =>
                  startRename(r.id, r.a.names[0] || shortId(r.id))
                }
              />
            ))}
            {pinnedChannels.map((r) => (
              <Row
                key={r.id}
                conv={r.conv}
                subtitle={r.subtitle}
                channel
                pinned
                onTogglePin={() => void togglePinned(r.id, false)}
                onRename={() => startRename(r.id, r.c.name)}
              />
            ))}
          </>
        )}

        <SectionLabel>{t("sidebar.directMessages")}</SectionLabel>
        {accounts.length === 0 && (
          <p className="px-2.5 py-2 text-xs text-muted-foreground">
            {t("sidebar.noContacts")}
          </p>
        )}
        {unpinnedAccounts.map((r) => (
          <Row
            key={r.id}
            conv={r.conv}
            subtitle={r.subtitle}
            pinned={false}
            onTogglePin={() => void togglePinned(r.id, true)}
            onRename={() => startRename(r.id, r.a.names[0] || shortId(r.id))}
          />
        ))}

        <SectionLabel action={<CreateChannelDialog />}>
          {t("sidebar.channels")}
        </SectionLabel>
        {channels.length === 0 && (
          <p className="px-2.5 py-2 text-xs text-muted-foreground">
            {t("sidebar.noChannels")}
          </p>
        )}
        {unpinnedChannels.map((r) => (
          <Row
            key={r.id}
            conv={r.conv}
            subtitle={r.subtitle}
            channel
            pinned={false}
            onTogglePin={() => void togglePinned(r.id, true)}
            onRename={() => startRename(r.id, r.c.name)}
          />
        ))}
      </nav>

      {/* Stranded prompt: nobody's online and the grace window has passed — offer the
          offline direct-connect guide. Quiet, dismissible, and only here when it's earned. */}
      {stranded && (
        <div
          data-testid="stranded-prompt"
          className="flex items-center gap-2 border-t border-signal/20 bg-signal/[0.06] px-2.5 py-1.5 text-xs"
        >
          <WifiOff className="h-3.5 w-3.5 shrink-0 text-signal" />
          <button
            type="button"
            data-testid="stranded-open"
            onClick={() => setOfflineOpen(true)}
            className="flex-1 truncate text-left font-medium text-foreground/90 hover:text-foreground hover:underline"
          >
            {noNetwork
              ? t("sidebar.strandedNoNetwork")
              : t("sidebar.strandedAlone")}
          </button>
          <button
            type="button"
            data-testid="stranded-dismiss"
            onClick={() => setStrandedDismissed(true)}
            title={t("common.dismiss")}
            className="shrink-0 rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}
      <OfflineConnectDialog
        open={offlineOpen}
        onOpenChange={setOfflineOpen}
        noNetwork={noNetwork}
      />

      {/* Bottom-left footer: the utility overflow menu (settings/diagnostics/etc.) sits
          first so the top stays clean, then the LAN status + brand mark. */}
      <div className="flex items-center gap-1.5 border-t px-2 py-2 text-xs text-muted-foreground">
        <UtilityMenu />
        {/* Active theme's crest/logo — ADDED in the bottom-left corner (the app's own mark
            stays at the far right; this is an addition, not a replacement). */}
        {themeCrest && (
          <img
            src={themeCrest}
            alt=""
            data-testid="theme-crest"
            title={t("settings.theme")}
            className="h-[18px] w-[18px] shrink-0 object-contain"
          />
        )}
        {ssid ? (
          <Wifi className="h-3.5 w-3.5 shrink-0 text-signal" />
        ) : (
          <Network className="h-3.5 w-3.5 shrink-0 text-signal" />
        )}
        <span
          className="flex-1 truncate"
          title={ssid ?? t("sidebar.localNetwork")}
        >
          {ssid ?? t("sidebar.localNetwork")}
        </span>
        {/* Live LAN headcount — restored from the old footer text; a breathing signal dot +
            the number of people currently discovered around us. Always visible (the SSID can
            grow long and truncate), with the full phrase in the tooltip. */}
        <span
          data-testid="lan-online-count"
          className="flex shrink-0 items-center gap-1 tabular-nums"
          title={t("sidebar.peopleOnLan", { count: onlinePeople })}
        >
          <PresenceDot
            status={onlinePeople > 0 ? "online" : "offline"}
            size="sm"
          />
          {onlinePeople}
        </span>
        {/* App's own brand mark — always present (kept distinct from the theme crest above). */}
        <Logo
          size={16}
          className="ml-1.5 shrink-0 opacity-80"
          title="Mesh-Talk"
        />
      </div>

      {/* Right-edge resize handle: drag to resize (persisted), double-click to reset. The
          hit area is a few px wide; a hairline highlights in the signal accent on hover. */}
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label={t("sidebar.resize")}
        tabIndex={0}
        data-testid="sidebar-resize-handle"
        onPointerDown={onPointerDown}
        onDoubleClick={reset}
        onKeyDown={onKeyDown}
        className="group absolute inset-y-0 right-0 z-20 w-1.5 translate-x-1/2 cursor-col-resize outline-none"
      >
        <span
          aria-hidden
          className="absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-transparent transition-colors group-hover:bg-signal group-focus-visible:bg-signal"
        />
      </div>

      <Dialog
        open={renameId !== null}
        onOpenChange={(o) => !o && setRenameId(null)}
      >
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>{t("sidebar.renameTitle")}</DialogTitle>
          </DialogHeader>
          <Input
            autoFocus
            value={renameDraft}
            onChange={(e) => setRenameDraft(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && commitRename()}
            placeholder={t("sidebar.aliasPlaceholder")}
          />
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setRenameId(null)}>
              {t("common.cancel")}
            </Button>
            <Button onClick={commitRename}>{t("common.save")}</Button>
          </div>
        </DialogContent>
      </Dialog>
    </aside>
  );
}
