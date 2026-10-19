import { useState } from "react";
import { Bookmark, MoreHorizontal, Pencil, Pin, PinOff } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Badge } from "@/components/ui/badge";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { IdentityGlyph, PresenceDot } from "@/components/identity";
import { GroupAvatar } from "@/components/GroupAvatar";
import { cn } from "@/lib/utils";
import { formatTime } from "@/lib/format";
import { convKey, useChat, type Conversation } from "@/store/chat";
import {
  presenceLabel,
  groupPresenceLabel,
  presenceStatus,
  usePresenceFor,
} from "@/store/presence";
import { ContactContextMenu } from "../HiddenContactsDialog";
import { MemberProfileDialog } from "../MemberProfileDialog";

export function ConversationRow({
  conv,
  subtitle,
  prioritizeSubtitle,
  channel,
  personal = false,
  pinned,
  onTogglePin,
  onRename,
  onHide,
  navIndex,
  listPosition,
  listSize,
}: {
  conv: Conversation;
  subtitle: string;
  prioritizeSubtitle?: boolean;
  channel?: boolean;
  personal?: boolean;
  pinned: boolean;
  onTogglePin: () => void;
  onRename: () => void;
  onHide?: () => void;
  navIndex?: number;
  listPosition: number;
  listSize: number;
}) {
  const { t } = useTranslation();
  const [actionsOpen, setActionsOpen] = useState(false);
  const [profileOpen, setProfileOpen] = useState(false);
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
    channel && !prioritizeSubtitle && !latest
      ? groupPresenceLabel(presence, t)
      : prioritizeSubtitle
        ? subtitle
        : (latest?.text ? latest.text : "") ||
          latest?.file?.name ||
          (personal
            ? subtitle
            : channel
              ? groupPresenceLabel(presence, t)
              : presenceLabel(presence, t));

  const row = (
    <div
      role="group"
      aria-label={`${listPosition} / ${listSize}`}
      className={cn(
        "conversation-row group relative flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-left",
        isActive ? "bg-signal/10" : "hover:bg-accent/45",
      )}
    >
      {!channel && !personal && (
        <button
          type="button"
          onClick={() => setProfileOpen(true)}
          data-testid={`contact-profile-${conv.id}`}
          aria-label={t("memberProfile.viewNamed", { name: conv.name })}
          className="relative shrink-0 rounded-full outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <IdentityGlyph seed={conv.id} size={36} title={conv.name} />
          <PresenceDot
            status={status}
            size="md"
            label={presenceLabel(presence, t)}
            className="pointer-events-none absolute -bottom-0.5 -right-0.5"
          />
        </button>
      )}
      <button
        onClick={() => open(conv)}
        data-conv-option
        data-nav-position={listPosition}
        data-virtual-index={navIndex}
        data-testid={`conversation-row-${conv.id}`}
        aria-current={isActive ? "true" : undefined}
        className="flex min-w-0 flex-1 items-center gap-2.5 rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        {personal && (
          <span
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-signal/10 text-signal"
            aria-hidden="true"
          >
            <Bookmark className="h-5 w-5" />
          </span>
        )}
        {channel && (
          <div className="relative shrink-0">
            <GroupAvatar channelId={conv.id} size={36} title={conv.name} />
          </div>
        )}
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
      {!personal && (
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
            {channel && onHide && (
              <button
                type="button"
                onClick={() => {
                  setActionsOpen(false);
                  onHide();
                }}
                className="flex w-full items-center rounded-md px-2 py-2 text-left text-sm hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                {t("sidebar.hideChannel")}
              </button>
            )}
          </PopoverContent>
        </Popover>
      )}
    </div>
  );
  if (channel || personal) return row;
  return (
    <>
      <ContactContextMenu account={conv.id} name={conv.name}>
        {row}
      </ContactContextMenu>
      <MemberProfileDialog
        target={profileOpen ? { name: conv.name, accountId: conv.id } : null}
        onOpenChange={setProfileOpen}
      />
    </>
  );
}
