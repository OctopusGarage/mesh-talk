import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  LogOut,
  Loader2,
  MoreHorizontal,
  Moon,
  Network,
  Wifi,
  WifiOff,
  Pencil,
  Pin,
  PinOff,
  Radar,
  Settings,
  Sun,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
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
import { formatTime, shortId } from "@/lib/format";
import { useTheme } from "@/lib/theme";
import { usePacks } from "@/store/packs";
import { CreateChannelDialog } from "./CreateChannelDialog";
import { SearchDialog } from "./SearchDialog";
import { FilesTray } from "./FilesTray";
import { LinkDeviceDialog } from "./LinkDeviceDialog";
import { DiagnosticsDialog } from "./DiagnosticsDialog";
import { WebRtcTestDialog } from "./WebRtcTestDialog";
import { OfflineConnectDialog } from "./OfflineConnectDialog";
import { SettingsDialog } from "./SettingsDialog";
import { ContactContextMenu } from "./HiddenContactsDialog";
import { useContactPolicy } from "@/store/contactPolicy";
import { ProfileDialog } from "./ProfileDialog";
import { AboutDialog } from "./AboutDialog";
import { useAuth } from "@/store/auth";
import { chat, diag } from "@/lib/api";
import { GroupAvatar } from "@/components/GroupAvatar";
import { convKey, useChat, type Conversation } from "@/store/chat";
import { useSettings } from "@/store/settings";
import {
  presenceLabel,
  presenceStatus,
  usePresence,
  usePresenceFor,
} from "@/store/presence";
import type { AccountInfo, ChannelInfo } from "@/lib/types";
import { CONNECTION_LABEL, connectionState } from "./connectionState";

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
  navIndex,
  listPosition,
  listSize,
}: {
  conv: Conversation;
  subtitle: string;
  channel?: boolean;
  pinned: boolean;
  onTogglePin: () => void;
  onRename: () => void;
  navIndex?: number;
  listPosition: number;
  listSize: number;
}) {
  const { t } = useTranslation();
  const [actionsOpen, setActionsOpen] = useState(false);
  const active = useChat((s) => s.active);
  const unread = useChat((s) => s.unread[convKey(conv)] ?? 0);
  const history = useChat((s) => s.messages[convKey(conv)]);
  const open = useChat((s) => s.open);
  const isActive = active != null && convKey(active) === convKey(conv);
  // Presence is read from the isolated store and keyed by id, so a presence tick only
  // re-renders the rows whose snapshot actually changed.
  const presence = usePresenceFor(conv.id);
  const status = presenceStatus(presence);
  const latest = history?.[history.length - 1];
  const lastTime = latest ? formatTime(latest.wallClock) : null;
  const summary =
    latest?.text ||
    latest?.file?.name ||
    (channel ? subtitle : presenceLabel(presence, t));

  const row = (
    <div
      role="group"
      aria-label={`${listPosition} / ${listSize}`}
      className={cn(
        "conversation-row group relative flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-left",
        isActive ? "bg-signal/10" : "hover:bg-accent/45",
      )}
    >
      <button
        onClick={() => open(conv)}
        data-conv-option
        data-nav-position={listPosition}
        data-virtual-index={navIndex}
        data-testid={`conversation-row-${conv.id}`}
        aria-current={isActive ? "true" : undefined}
        aria-label={`${conv.name}${summary ? `, ${summary}` : ""}`}
        className="flex min-w-0 flex-1 items-center gap-2.5 rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <div className="relative shrink-0">
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
          <div className="flex min-w-0 items-baseline gap-2">
            <span
              title={conv.name}
              className={cn(
                "min-w-0 flex-1 truncate text-[13px] leading-5",
                unread || isActive ? "font-semibold" : "font-medium",
              )}
            >
              {conv.name}
            </span>
            {lastTime && (
              <span className="sidebar-row-time shrink-0 text-[11px] tabular-nums text-muted-foreground group-hover:opacity-0 group-focus-within:opacity-0">
                {lastTime}
              </span>
            )}
          </div>
          <div
            className="truncate text-[11px] leading-4 text-muted-foreground"
            title={summary}
          >
            {summary}
          </div>
        </div>
      </button>
      {unread > 0 && (
        <Badge className="bg-signal font-mono text-[11px] text-primary-foreground group-hover:opacity-0 group-focus-within:opacity-0">
          {unread}
        </Badge>
      )}
      <Popover open={actionsOpen} onOpenChange={setActionsOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            data-testid={`conversation-actions-${conv.id}`}
            title={t("sidebar.moreActions")}
            aria-label={`${t("sidebar.moreActions")} ${conv.name}`}
            className={cn(
              "absolute right-1 top-1/2 flex h-8 w-8 -translate-y-1/2 items-center justify-center rounded-md bg-card text-muted-foreground shadow-sm hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [@media(hover:none)]:h-11 [@media(hover:none)]:w-11 [@media(hover:none)]:opacity-100",
              actionsOpen
                ? "opacity-100"
                : "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100",
            )}
          >
            <MoreHorizontal className="h-4 w-4" />
          </button>
        </PopoverTrigger>
        <PopoverContent align="end" className="w-44 p-1">
          <button
            type="button"
            onClick={() => {
              setActionsOpen(false);
              onRename();
            }}
            data-testid={`conversation-rename-${conv.id}`}
            aria-label={`${t("sidebar.rename")} ${conv.name}`}
            className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <Pencil className="h-3.5 w-3.5" />
            {t("sidebar.rename")}
          </button>
          <button
            type="button"
            onClick={() => {
              setActionsOpen(false);
              onTogglePin();
            }}
            data-testid={`conversation-pin-${conv.id}`}
            aria-label={`${t(pinned ? "sidebar.unpin" : "sidebar.pin")} ${conv.name}`}
            className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {pinned ? (
              <PinOff className="h-3.5 w-3.5" />
            ) : (
              <Pin className="h-3.5 w-3.5" />
            )}
            {t(pinned ? "sidebar.unpin" : "sidebar.pin")}
          </button>
        </PopoverContent>
      </Popover>
    </div>
  );
  return channel ? (
    row
  ) : (
    <ContactContextMenu account={conv.id} name={conv.name}>
      {row}
    </ContactContextMenu>
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
    <div className="flex items-center justify-between px-2.5 pb-1.5 pt-4 text-[10px] font-semibold uppercase tracking-[0.11em] text-muted-foreground">
      <span>{children}</span>
      {action}
    </div>
  );
}

// Sidebar width: user-resizable via the right-edge drag handle, persisted to localStorage,
// clamped to a sensible range, double-click to reset. Default mirrors the old `w-72`.
const WIDTH_KEY = "mesh-talk-sidebar-width";
const DEFAULT_WIDTH = 284;
const MIN_WIDTH = 230;
const MAX_WIDTH = 460;
const VIRTUALIZE_AT = 80;
// Match the row's minimum height so virtualized lists preserve focus and scroll math.
const CONVERSATION_ROW_HEIGHT = 56;

const clampWidth = (w: number) =>
  Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(w)));

function readWidth(): number {
  if (typeof localStorage === "undefined") return DEFAULT_WIDTH;
  const v = Number(localStorage.getItem(WIDTH_KEY));
  return Number.isFinite(v) && v > 0 ? clampWidth(v) : DEFAULT_WIDTH;
}

function useSidebarWidth() {
  const [width, setWidth] = useState(readWidth);
  const dragCleanup = useRef<(() => void) | null>(null);

  const persist = useCallback((w: number) => {
    setWidth(w);
    if (typeof localStorage !== "undefined")
      localStorage.setItem(WIDTH_KEY, String(w));
  }, []);

  useEffect(() => () => dragCleanup.current?.(), []);

  // Pointer capture keeps the drag continuous outside the narrow handle. Persist only
  // the settled width so a long drag does not write storage on every pointer event.
  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (e.button !== 0 || dragCleanup.current) return;
      e.preventDefault();
      const handle = e.currentTarget as HTMLElement;
      const pointerId = e.pointerId;
      const aside = (e.currentTarget as HTMLElement).parentElement;
      const left = aside?.getBoundingClientRect().left ?? 0;
      const prevCursor = document.body.style.cursor;
      const prevSelect = document.body.style.userSelect;
      let latest = clampWidth(e.clientX - left);
      let done = false;
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      handle.setPointerCapture(pointerId);
      const onMove = (ev: PointerEvent) => {
        if (ev.pointerId !== pointerId || done) return;
        latest = clampWidth(ev.clientX - left);
        setWidth(latest);
      };
      const cleanup = () => {
        if (done) return;
        done = true;
        document.body.style.cursor = prevCursor;
        document.body.style.userSelect = prevSelect;
        handle.removeEventListener("pointermove", onMove);
        handle.removeEventListener("pointerup", onEnd);
        handle.removeEventListener("pointercancel", onEnd);
        handle.removeEventListener("lostpointercapture", onEnd);
        window.removeEventListener("blur", onEnd);
        if (handle.hasPointerCapture(pointerId))
          handle.releasePointerCapture(pointerId);
        if (dragCleanup.current === cleanup) dragCleanup.current = null;
      };
      const onEnd = (ev?: PointerEvent | Event) => {
        if (ev instanceof PointerEvent && ev.pointerId !== pointerId) return;
        cleanup();
        persist(latest);
      };
      dragCleanup.current = cleanup;
      handle.addEventListener("pointermove", onMove);
      handle.addEventListener("pointerup", onEnd);
      handle.addEventListener("pointercancel", onEnd);
      handle.addEventListener("lostpointercapture", onEnd);
      window.addEventListener("blur", onEnd);
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

/** Secondary actions share one labeled utility menu beside connection status. */
function UtilityMenu() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [connectionOpen, setConnectionOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const theme = useTheme((s) => s.theme);
  const toggleTheme = useTheme((s) => s.toggle);
  const logout = useAuth((s) => s.logout);
  // The WebRTC self-test only makes sense for the calls feature, so it rides the same gate.
  const callsEnabled = useSettings((s) => s.callsEnabled);

  return (
    <>
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
          className="w-56 p-1.5"
          data-testid="sidebar-overflow-menu"
        >
          <div className="grid gap-0.5">
            <LinkDeviceDialog menuItem />
            <Button
              variant="ghost"
              size="sm"
              className="w-full justify-start gap-2.5 px-2"
              data-testid="sidebar-nav-connection"
              onClick={() => {
                setOpen(false);
                setConnectionOpen(true);
              }}
            >
              <Radar className="h-4 w-4" />
              {t("redesign.connection")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className="w-full justify-start gap-2.5 px-2"
              data-testid="sidebar-nav-settings"
              onClick={() => {
                setOpen(false);
                setSettingsOpen(true);
              }}
            >
              <Settings className="h-4 w-4" />
              {t("settings.title")}
            </Button>
            {callsEnabled && <WebRtcTestDialog menuItem />}
            <AboutDialog menuItem />
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
      <DiagnosticsDialog
        open={connectionOpen}
        onOpenChange={setConnectionOpen}
      />
      <SettingsDialog open={settingsOpen} onOpenChange={setSettingsOpen} />
    </>
  );
}

export function Sidebar() {
  const { t } = useTranslation();
  const rawAccounts = useChat((s) => s.accounts);
  const hiddenContacts = useContactPolicy((s) => s.contacts);
  const policyLoaded = useContactPolicy((s) => s.loaded);
  const policyError = useContactPolicy((s) => s.error);
  const owner = useAuth((s) => s.user?.id);
  const accounts = useMemo(
    () =>
      policyLoaded
        ? rawAccounts.filter((a) => !hiddenContacts[a.account_id])
        : [],
    [rawAccounts, hiddenContacts, policyLoaded],
  );
  const channels = useChat((s) => s.channels);
  const peers = useChat((s) => s.peers);
  const favorites = useChat((s) => s.favorites);
  const selectedTheme = useTheme((s) => s.theme);
  const themePacks = usePacks((s) => s.packs);
  const activeThemePack = themePacks.find((pack) => pack.id === selectedTheme);
  const themeCrest =
    activeThemePack?.kind === "theme" ? activeThemePack.crest : undefined;
  // The active theme's crest (a brand theme) replaces the app mark in the footer, so the
  // chosen club/national/Messi identity is always present without shouting.
  const togglePinned = useChat((s) => s.togglePinned);
  const setAlias = useChat((s) => s.setAlias);
  const renameChannel = useChat((s) => s.renameChannel);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const ready = useChat((s) => s.ready);
  // Online people on the LAN — distinct OTHER accounts that are presence-ONLINE (the same
  // signal the conversation-row dots use, so the count always matches the visible green dots).
  // Excludes post-office relays (infrastructure, not people) and our own other devices, and
  // ignores roster entries that are merely lingering (seen, but past the online window) so the
  // number doesn't drift from what's actually online.
  const presenceMap = usePresence((s) => s.map);
  const networkPeople = useMemo(() => {
    const online = new Set<string>();
    for (const p of peers) {
      if (p.post_office) continue;
      const account = p.account_id ?? p.user_id;
      if (account === myAccountId) continue;
      if (presenceMap[account]?.online) online.add(account);
    }
    return online.size;
  }, [peers, presenceMap, myAccountId]);
  const onlinePeople = useMemo(() => {
    if (!policyLoaded) return 0;
    const online = new Set<string>();
    for (const p of peers) {
      const account = p.account_id ?? p.user_id;
      if (
        !p.post_office &&
        account !== myAccountId &&
        !hiddenContacts[account] &&
        presenceMap[account]?.online
      )
        online.add(account);
    }
    return online.size;
  }, [peers, presenceMap, myAccountId, hiddenContacts, policyLoaded]);
  const bootFailed = useChat((s) => s.bootFailed);
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
  const connection = connectionState({
    bootFailed,
    ready,
    noNetwork,
    onlinePeople,
  });
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
    ready && graceElapsed && networkPeople === 0 && !strandedDismissed;

  // Roving keyboard navigation across the conversation rows. Arrow Up/Down moves focus
  // between the option buttons within the list (Enter/Space already open via the native
  // button). Scoped to the nav so it never traps the composer or other controls.
  const navRef = useRef<HTMLElement>(null);
  const [navScrollParent, setNavScrollParent] = useState<HTMLElement | null>(
    null,
  );
  const virtualPinnedRef = useRef<VirtuosoHandle>(null);
  const virtualAccountsRef = useRef<VirtuosoHandle>(null);
  const virtualChannelsRef = useRef<VirtuosoHandle>(null);
  const pendingVirtualFocus = useRef<number | null>(null);
  const attachNav = useCallback((node: HTMLElement | null) => {
    navRef.current = node;
    setNavScrollParent(node);
  }, []);
  const onNavKeyDown = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    if (
      !(e.target instanceof HTMLButtonElement) ||
      !e.target.matches("[data-conv-option]")
    )
      return;
    const position = Number(e.target.dataset.navPosition);
    const next = Math.max(
      1,
      Math.min(listSize, position + (e.key === "ArrowDown" ? 1 : -1)),
    );
    e.preventDefault();
    const visible = navRef.current?.querySelector<HTMLButtonElement>(
      `[data-nav-position="${next}"]`,
    );
    if (visible) {
      pendingVirtualFocus.current = null;
      visible.focus();
      return;
    }
    pendingVirtualFocus.current = next;
    if (next <= pinnedCount) {
      virtualPinnedRef.current?.scrollToIndex({
        index: next - 1,
        align: "center",
      });
    } else if (next <= pinnedCount + unpinnedAccounts.length) {
      virtualAccountsRef.current?.scrollToIndex({
        index: next - pinnedCount - 1,
        align: "center",
      });
    } else {
      virtualChannelsRef.current?.scrollToIndex({
        index: next - pinnedCount - unpinnedAccounts.length - 1,
        align: "center",
      });
    }
  };

  // A channel I own: renaming it changes the shared (synced) name for everyone. A contact —
  // or a channel I don't own (the core would reject the change) — gets a personal alias
  // instead. Single source of truth for that distinction, used by the rename flow + dialog.
  const ownedChannel = (id: string) =>
    channels.find((c) => c.channel_id === id && c.owner === myId);

  const startRename = (id: string, current: string) => {
    setRenameId(id);
    // For a channel I own we edit the real (synced) name; otherwise the personal alias.
    setRenameDraft(
      ownedChannel(id) ? current : (favorites[id]?.custom_alias ?? current),
    );
  };
  const commitRename = () => {
    if (renameId) {
      const owned = ownedChannel(renameId);
      if (owned) {
        const next = renameDraft.trim();
        if (next && next !== owned.name) void renameChannel(renameId, next);
      } else {
        void setAlias(renameId, renameDraft);
      }
    }
    setRenameId(null);
  };
  // The dialog's title + placeholder switch to channel wording when renaming a channel I own.
  const renamingOwnedChannel =
    renameId !== null && ownedChannel(renameId) !== undefined;

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
        subtitle: t("sidebar.devices", { count: a.device_count }),
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
        subtitle: t("conversation.members", { count: c.member_count }),
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
  const listSize = accounts.length + channels.length;
  const pinnedCount = pinnedAccounts.length + pinnedChannels.length;
  const pinnedRows = useMemo(
    () => [
      ...pinnedAccounts.map((r) => ({
        id: r.id,
        conv: r.conv,
        subtitle: r.subtitle,
        channel: false,
        renameName: r.a.names[0] || shortId(r.id),
      })),
      ...pinnedChannels.map((r) => ({
        id: r.id,
        conv: r.conv,
        subtitle: r.subtitle,
        channel: true,
        renameName: r.c.name,
      })),
    ],
    [pinnedAccounts, pinnedChannels],
  );
  const focusVirtualRow = (
    {
      startIndex,
      endIndex,
    }: {
      startIndex: number;
      endIndex: number;
    },
    firstPosition: number,
  ) => {
    const pending = pendingVirtualFocus.current;
    if (
      pending === null ||
      pending < firstPosition + startIndex ||
      pending > firstPosition + endIndex
    )
      return;
    requestAnimationFrame(() => {
      if (pendingVirtualFocus.current !== pending) return;
      const target = navRef.current?.querySelector<HTMLButtonElement>(
        `[data-nav-position="${pending}"]`,
      );
      if (
        target &&
        (document.activeElement === document.body ||
          navRef.current?.contains(document.activeElement))
      ) {
        target.focus();
        pendingVirtualFocus.current = null;
      }
    });
  };

  return (
    <aside
      data-testid="sidebar"
      style={{ width }}
      className="sidebar-container relative flex shrink-0 flex-col border-r bg-[hsl(var(--shell-rail))]"
    >
      {/* Conversation title and search live above the list. The drag region and
          titlebar inset leave room for native window controls. */}
      <div
        className="sidebar-heading border-b px-3.5 pb-3 pt-4"
        data-testid="self-identity"
        data-tauri-drag-region
        data-titlebar-inset="left"
      >
        <div className="mb-3 flex items-center gap-2.5 font-display text-[16px] font-semibold tracking-tight">
          <Logo size={27} />
          <span>Mesh-Talk</span>
        </div>
        <SearchDialog />
        <ProfileDialog open={profileOpen} onOpenChange={setProfileOpen} />
      </div>

      <nav
        data-testid="conversation-nav"
        ref={attachNav}
        aria-label={t("conversation.list")}
        onKeyDown={onNavKeyDown}
        onPointerDownCapture={() => {
          pendingVirtualFocus.current = null;
        }}
        className="flex-1 overflow-y-auto px-2 pb-3"
      >
        {hasPinned && (
          <>
            <SectionLabel>{t("sidebar.pinned")}</SectionLabel>
            {pinnedRows.length > VIRTUALIZE_AT ? (
              <Virtuoso
                ref={virtualPinnedRef}
                data={pinnedRows}
                customScrollParent={navScrollParent ?? undefined}
                defaultItemHeight={CONVERSATION_ROW_HEIGHT}
                overscan={250}
                style={{ height: pinnedRows.length * CONVERSATION_ROW_HEIGHT }}
                computeItemKey={(_, r) => r.id}
                rangeChanged={(range) => focusVirtualRow(range, 1)}
                itemContent={(index, r) => (
                  <Row
                    conv={r.conv}
                    subtitle={r.subtitle}
                    channel={r.channel}
                    pinned
                    navIndex={index}
                    listPosition={index + 1}
                    listSize={listSize}
                    onTogglePin={() => void togglePinned(r.id, false)}
                    onRename={() => startRename(r.id, r.renameName)}
                  />
                )}
              />
            ) : (
              pinnedRows.map((r, index) => (
                <Row
                  key={r.id}
                  conv={r.conv}
                  subtitle={r.subtitle}
                  channel={r.channel}
                  pinned
                  listPosition={index + 1}
                  listSize={listSize}
                  onTogglePin={() => void togglePinned(r.id, false)}
                  onRename={() => startRename(r.id, r.renameName)}
                />
              ))
            )}
          </>
        )}

        <SectionLabel>{t("sidebar.directMessages")}</SectionLabel>
        {!policyLoaded && (
          <div
            className="px-2.5 py-2 text-xs text-muted-foreground"
            role={policyError ? "alert" : "status"}
          >
            {t(policyError ? "contactVisibility.loadError" : "common.loading")}
            {policyError && (
              <button
                type="button"
                className="ml-2 underline"
                onClick={() => {
                  if (owner) void useContactPolicy.getState().load(owner);
                }}
              >
                {t("contactVisibility.retry")}
              </button>
            )}
          </div>
        )}
        {policyLoaded && accounts.length === 0 && (
          <p className="px-2.5 py-2 text-xs text-muted-foreground">
            {t(
              rawAccounts.length > 0
                ? "contactVisibility.allHidden"
                : "sidebar.noContacts",
            )}
          </p>
        )}
        {unpinnedAccounts.length > VIRTUALIZE_AT ? (
          <Virtuoso
            ref={virtualAccountsRef}
            data={unpinnedAccounts}
            customScrollParent={navScrollParent ?? undefined}
            defaultItemHeight={CONVERSATION_ROW_HEIGHT}
            overscan={250}
            style={{
              height: unpinnedAccounts.length * CONVERSATION_ROW_HEIGHT,
            }}
            computeItemKey={(_, r) => r.id}
            rangeChanged={(range) => focusVirtualRow(range, pinnedCount + 1)}
            itemContent={(index, r) => (
              <Row
                conv={r.conv}
                subtitle={r.subtitle}
                pinned={false}
                navIndex={index}
                listPosition={pinnedCount + index + 1}
                listSize={listSize}
                onTogglePin={() => void togglePinned(r.id, true)}
                onRename={() =>
                  startRename(r.id, r.a.names[0] || shortId(r.id))
                }
              />
            )}
          />
        ) : (
          unpinnedAccounts.map((r, index) => (
            <Row
              key={r.id}
              conv={r.conv}
              subtitle={r.subtitle}
              pinned={false}
              listPosition={pinnedCount + index + 1}
              listSize={listSize}
              onTogglePin={() => void togglePinned(r.id, true)}
              onRename={() => startRename(r.id, r.a.names[0] || shortId(r.id))}
            />
          ))
        )}

        <SectionLabel action={<CreateChannelDialog />}>
          {t("sidebar.channels")}
        </SectionLabel>
        {channels.length === 0 && (
          <p className="px-2.5 py-2 text-xs text-muted-foreground">
            {t("sidebar.noChannels")}
          </p>
        )}
        {unpinnedChannels.length > VIRTUALIZE_AT ? (
          <Virtuoso
            ref={virtualChannelsRef}
            data={unpinnedChannels}
            customScrollParent={navScrollParent ?? undefined}
            defaultItemHeight={CONVERSATION_ROW_HEIGHT}
            overscan={250}
            style={{
              height: unpinnedChannels.length * CONVERSATION_ROW_HEIGHT,
            }}
            computeItemKey={(_, r) => r.id}
            rangeChanged={(range) =>
              focusVirtualRow(range, pinnedCount + unpinnedAccounts.length + 1)
            }
            itemContent={(index, r) => (
              <Row
                conv={r.conv}
                subtitle={r.subtitle}
                channel
                pinned={false}
                navIndex={index}
                listPosition={pinnedCount + unpinnedAccounts.length + index + 1}
                listSize={listSize}
                onTogglePin={() => void togglePinned(r.id, true)}
                onRename={() => startRename(r.id, r.c.name)}
              />
            )}
          />
        ) : (
          unpinnedChannels.map((r, index) => (
            <Row
              key={r.id}
              conv={r.conv}
              subtitle={r.subtitle}
              channel
              pinned={false}
              listPosition={pinnedCount + unpinnedAccounts.length + index + 1}
              listSize={listSize}
              onTogglePin={() => void togglePinned(r.id, true)}
              onRename={() => startRename(r.id, r.c.name)}
            />
          ))
        )}
      </nav>

      <div
        className="sidebar-tools border-t px-2 py-1.5"
        aria-label={t("redesign.tools")}
      >
        <FilesTray navigation />
      </div>

      {/* Stranded prompt: nobody's online and the grace window has passed — offer the
          offline direct-connect guide. Quiet, dismissible, and only here when it's earned. */}
      {stranded && (
        <div
          data-testid="stranded-prompt"
          className={cn(
            "flex items-center gap-2 border-t px-2.5 py-1.5 text-xs",
            noNetwork
              ? "border-attention/25 bg-attention/[0.07]"
              : "border-signal/20 bg-signal/[0.06]",
          )}
        >
          <WifiOff
            className={cn(
              "h-3.5 w-3.5 shrink-0",
              noNetwork ? "text-attention" : "text-signal",
            )}
          />
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
            aria-label={t("common.dismiss")}
            className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [@media(hover:none)]:h-11 [@media(hover:none)]:w-11"
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

      <div className="border-t px-2.5 py-2 text-[11px] text-muted-foreground">
        <button
          type="button"
          data-testid="open-profile"
          onClick={() => setProfileOpen(true)}
          aria-label={t("profile.open")}
          className="mb-1 flex min-h-10 w-full items-center gap-2.5 rounded-md px-1 text-left hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <IdentityGlyph
            seed={myAccountId || myId || username}
            size={34}
            title={displayName}
          />
          <span className="min-w-0 flex-1">
            <span
              data-testid="sidebar-own-name"
              className="block truncate text-sm font-semibold text-foreground"
            >
              {displayName}
            </span>
            <span className="block truncate text-xs text-muted-foreground">
              {t("redesign.localIdentity")}
            </span>
          </span>
        </button>
        <div className="flex items-center gap-2 font-medium text-foreground">
          {connection === "starting" || connection === "searching" ? (
            <Loader2
              aria-hidden="true"
              className="h-3.5 w-3.5 shrink-0 animate-spin text-signal motion-reduce:animate-none"
            />
          ) : (
            <PresenceDot
              status={connection === "ready" ? "online" : "offline"}
              size="sm"
            />
          )}
          <span
            data-testid="connection-status"
            className={cn(
              "min-w-0 flex-1 truncate",
              connection === "failed" && "text-destructive",
              connection === "no-network" && "text-attention",
            )}
            title={t(CONNECTION_LABEL[connection])}
          >
            {t(CONNECTION_LABEL[connection])}
          </span>
          <UtilityMenu />
        </div>
        <div className="mt-1 flex items-center gap-1.5 pl-4.5">
          {ssid ? (
            <Wifi className="h-3.5 w-3.5 shrink-0" />
          ) : (
            <Network className="h-3.5 w-3.5 shrink-0" />
          )}
          <span
            className="min-w-0 flex-1 truncate"
            title={ssid ?? t("sidebar.localNetwork")}
          >
            {ssid ?? t("sidebar.localNetwork")}
          </span>
          {themeCrest && (
            <img
              src={themeCrest}
              alt=""
              data-testid="theme-crest"
              title={t("settings.theme")}
              className="h-4 w-4 shrink-0 object-contain"
            />
          )}
          {/* Live LAN headcount — restored from the old footer text; a breathing signal dot +
            the number of people currently discovered around us. Always visible (the SSID can
            grow long and truncate), with the full phrase in the tooltip. */}
          <span
            data-testid="lan-online-count"
            className="flex shrink-0 items-center gap-1 tabular-nums"
            title={t("sidebar.peopleOnLan", { count: onlinePeople })}
            aria-label={t("sidebar.peopleOnLan", { count: onlinePeople })}
          >
            <PresenceDot
              status={onlinePeople > 0 ? "online" : "offline"}
              size="sm"
            />
            {t("sidebar.onlineShort", { count: onlinePeople })}
          </span>
          {/* App's own brand mark — always present (kept distinct from the theme crest above). */}
          <Logo
            size={16}
            className="ml-1.5 shrink-0 opacity-60"
            title="Mesh-Talk"
          />
        </div>
      </div>

      {/* Right-edge resize handle: drag to resize (persisted), double-click to reset. The
          hit area is a few px wide; a hairline highlights in the signal accent on hover. */}
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label={t("sidebar.resize")}
        aria-valuemin={MIN_WIDTH}
        aria-valuemax={MAX_WIDTH}
        aria-valuenow={width}
        aria-valuetext={`${width} px`}
        tabIndex={0}
        data-testid="sidebar-resize-handle"
        onPointerDown={onPointerDown}
        onDoubleClick={reset}
        onKeyDown={onKeyDown}
        className="group absolute inset-y-0 right-0 z-20 w-1.5 translate-x-1/2 touch-none cursor-col-resize outline-none"
      >
        <span
          aria-hidden
          className="absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-transparent transition-[background-color,width] group-hover:bg-signal group-focus-visible:w-[3px] group-focus-visible:bg-signal"
        />
      </div>

      <Dialog
        open={renameId !== null}
        onOpenChange={(o) => !o && setRenameId(null)}
      >
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>
              {renamingOwnedChannel
                ? t("sidebar.renameChannelTitle")
                : t("sidebar.renameTitle")}
            </DialogTitle>
          </DialogHeader>
          <Input
            autoFocus
            value={renameDraft}
            onChange={(e) => setRenameDraft(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && commitRename()}
            placeholder={
              renamingOwnedChannel
                ? t("sidebar.channelNamePlaceholder")
                : t("sidebar.aliasPlaceholder")
            }
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
