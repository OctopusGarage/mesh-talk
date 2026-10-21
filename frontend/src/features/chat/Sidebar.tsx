import { useEffect, useMemo, useState } from "react";
import { Loader2, Network, Wifi, WifiOff, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { IdentityGlyph, PresenceDot } from "@/components/identity";
import { Logo } from "@/components/Logo";
import { cn } from "@/lib/utils";
import { useTheme } from "@/lib/theme";
import { usePacks } from "@/store/packs";
import { CreateChannelDialog } from "./CreateChannelDialog";
import { SearchDialog } from "./SearchDialog";
import { FilesTray } from "./FilesTray";
import { OfflineConnectDialog } from "./OfflineConnectDialog";
import { useContactPolicy } from "@/store/contactPolicy";
import { ProfileDialog } from "./ProfileDialog";
import { useAuth } from "@/store/auth";
import { chat, diag } from "@/lib/api";
import { useChat } from "@/store/chat";
import { usePrivacy } from "@/store/privacy";
import { usePresence } from "@/store/presence";
import {
  conversationList,
  sidebarPeopleCounts,
} from "./sidebar/conversationList";
import { ConversationSection } from "./sidebar/ConversationSection";
import { SidebarUtilities } from "./sidebar/SidebarUtilities";
import {
  MIN_WIDTH,
  MAX_WIDTH,
  useSidebarWidth,
} from "./sidebar/useSidebarWidth";
import { useConversationNavigation } from "./sidebar/useConversationNavigation";
import { useSerialPoll } from "./useSerialPoll";
import { CONNECTION_LABEL, connectionState } from "./connectionState";
import { PresenceAvailability } from "./PresenceAvailability";

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

export function Sidebar() {
  const { t } = useTranslation();
  const rawAccounts = useChat((s) => s.accounts);
  const hiddenContacts = useContactPolicy((s) => s.contacts);
  const policyLoaded = useContactPolicy((s) => s.loaded);
  const policyError = useContactPolicy((s) => s.error);
  const owner = useAuth((s) => s.user?.id);
  const publiclyVisible = usePrivacy(
    (s) => s.owner === owner && s.loaded && s.snapshot?.invisible === false,
  );
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
  const setChannelHidden = useChat((s) => s.setChannelHidden);
  const setAlias = useChat((s) => s.setAlias);
  const renameChannel = useChat((s) => s.renameChannel);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const ready = useChat((s) => s.ready);
  // Known online people — distinct OTHER accounts that are presence-ONLINE (the same
  // signal the conversation-row dots use, so the count always matches the visible green dots).
  // Excludes post-office relays (infrastructure, not people) and our own other devices, and
  // ignores roster entries that are merely lingering (seen, but past the online window) so the
  // number doesn't drift from what's actually online.
  const presenceMap = usePresence((s) => s.map);
  const { networkPeople, onlinePeople } = useMemo(
    () =>
      sidebarPeopleCounts(
        peers,
        presenceMap,
        myAccountId,
        hiddenContacts,
        policyLoaded,
      ),
    [peers, presenceMap, myAccountId, hiddenContacts, policyLoaded],
  );
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
  // Share the same non-overlapping polling lifecycle as other live views.
  useSerialPoll(
    true,
    60_000,
    () => chat.networkName(),
    setSsid,
    () => {},
  );
  useSerialPoll(
    true,
    60_000,
    () => diag.networkInfo(),
    (info) => setNoNetwork(info.interfaces.length === 0),
    () => {},
  );

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

  const { pinnedRows, accountRows, channelRows, hiddenChannels, listSize } =
    useMemo(
      () =>
        conversationList(accounts, channels, favorites, t, myAccountId, ready),
      [accounts, channels, favorites, t, ready, myAccountId],
    );
  const pinnedCount = pinnedRows.length;
  const {
    attachNav,
    navScrollParent,
    onNavKeyDown,
    virtualPinnedRef,
    virtualAccountsRef,
    virtualChannelsRef,
    focusVirtualRow,
    cancelPendingFocus,
  } = useConversationNavigation(pinnedCount, accountRows.length, listSize);
  const sectionProps = {
    listSize,
    scrollParent: navScrollParent,
    onRangeChanged: focusVirtualRow,
    onTogglePin: togglePinned,
    onRename: startRename,
    onHide: setChannelHidden,
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
        onPointerDownCapture={cancelPendingFocus}
        className="flex-1 overflow-y-auto px-2 pb-3"
      >
        {pinnedRows.length > 0 && (
          <>
            <SectionLabel>{t("sidebar.pinned")}</SectionLabel>
            <ConversationSection
              {...sectionProps}
              rows={pinnedRows}
              firstPosition={1}
              listRef={virtualPinnedRef}
            />
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
        <ConversationSection
          {...sectionProps}
          rows={accountRows}
          firstPosition={pinnedCount + 1}
          listRef={virtualAccountsRef}
        />

        <SectionLabel action={<CreateChannelDialog />}>
          {t("sidebar.channels")}
        </SectionLabel>
        {channels.length === 0 && (
          <p className="px-2.5 py-2 text-xs text-muted-foreground">
            {t("sidebar.noChannels")}
          </p>
        )}
        <ConversationSection
          {...sectionProps}
          rows={channelRows}
          firstPosition={pinnedCount + accountRows.length + 1}
          listRef={virtualChannelsRef}
        />
        {hiddenChannels.length > 0 && (
          <details className="px-2.5 pt-3 text-xs text-muted-foreground">
            <summary className="cursor-pointer rounded-md py-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
              {t("sidebar.hiddenChannels", { count: hiddenChannels.length })}
            </summary>
            <div className="space-y-1 pb-2">
              {hiddenChannels.map((row) => (
                <div
                  key={row.id}
                  className="flex items-center justify-between gap-2 rounded-md px-2 py-1.5"
                >
                  <span className="min-w-0 truncate" title={row.conv.name}>
                    {row.conv.name}
                  </span>
                  <button
                    type="button"
                    className="shrink-0 rounded-md px-2 py-1 text-signal hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    onClick={() => void setChannelHidden(row.id, false)}
                  >
                    {t("sidebar.restoreChannel")}
                  </button>
                </div>
              ))}
            </div>
          </details>
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
          title={t("profile.open")}
          className="mb-1 flex min-h-10 w-full items-center gap-2.5 rounded-md px-1 text-left hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <span className="sr-only">{t("profile.open")}</span>
          <span className="relative shrink-0">
            <IdentityGlyph
              seed={myAccountId || myId || username}
              size={34}
              title={displayName}
            />
            <PresenceDot
              status={ready && publiclyVisible ? "online" : "offline"}
              size="md"
              className="pointer-events-none absolute -bottom-0.5 -right-0.5"
            />
          </span>
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
          <SidebarUtilities />
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
          {/* Client-relative headcount — restored from the old footer text; a breathing signal dot +
            the number of people currently observed by this client. Always visible (the SSID can
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
        <PresenceAvailability />
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
