import { useLayoutEffect, useRef, type KeyboardEvent } from "react";
import {
  ArrowDownToLine,
  ArrowLeft,
  ArrowRight,
  CheckCheck,
  Clock3,
  FileText,
  Fingerprint,
  FolderOpen,
  Paperclip,
  Plus,
  Search,
  Send,
  ShieldQuestion,
  Smile,
  Wifi,
  X,
} from "lucide-react";
import { type Conversation, type Demo, type Message } from "./model";

export function Mark({ small = false }: { small?: boolean }) {
  return (
    <span
      className={`mark ${small ? "mark-small" : ""}`}
      aria-label="Mesh-Talk"
    >
      ⌘<span>·</span>
    </span>
  );
}

export function Avatar({
  person,
  size = "normal",
}: {
  person: Conversation;
  size?: "normal" | "large";
}) {
  return (
    <span
      className={`avatar avatar-${size} ${person.kind === "channel" ? "avatar-channel" : ""}`}
      aria-label={person.name}
    >
      {person.initials}
      <span
        className={`presence ${person.presence.startsWith("Online") ? "online" : person.presence.startsWith("Last") ? "recent" : ""}`}
      />
    </span>
  );
}

export function ConversationItems({
  demo,
  variant,
}: {
  demo: Demo;
  variant: "a" | "b" | "c";
}) {
  return (
    <div className="conversation-items" role="list" aria-label="Conversations">
      {demo.visible.map((person) => (
        <button
          key={person.id}
          className={`conversation-item ${person.id === demo.selected ? "selected" : ""}`}
          aria-current={person.id === demo.selected ? "true" : undefined}
          onClick={() => demo.setSelected(person.id)}
        >
          <Avatar person={person} />
          <span className="item-text">
            <span className="item-top">
              <strong>{person.name}</strong>
              <time>{person.time}</time>
            </span>
            <span className="item-bottom">
              <span>
                {variant === "b" && person.kind === "person"
                  ? person.presence
                  : person.preview}
              </span>
              {person.unread && <b className="unread">{person.unread}</b>}
            </span>
          </span>
        </button>
      ))}
      {demo.visible.length === 0 && (
        <p className="list-empty">No conversations match “{demo.query}”.</p>
      )}
    </div>
  );
}

export function MessageList({
  demo,
  variant,
}: {
  demo: Demo;
  variant: "a" | "b" | "c";
}) {
  const scroll = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight;
  }, [demo.selected, demo.messages.length]);
  return (
    <div
      ref={scroll}
      className="message-scroll"
      role="log"
      aria-label={`Messages with ${demo.current.name}`}
    >
      <div className="message-stack">
        <div className="date-marker">
          <span>Today · 15 November</span>
        </div>
        {demo.messages.map((m) => (
          <MessageItem key={m.id} m={m} demo={demo} variant={variant} />
        ))}
      </div>
    </div>
  );
}

function MessageItem({
  m,
  demo,
  variant,
}: {
  m: Message;
  demo: Demo;
  variant: "a" | "b" | "c";
}) {
  return (
    <article
      className={`message ${m.mine ? "mine" : "theirs"} ${m.file ? "file" : ""}`}
    >
      {variant === "b" && <span className="message-author">{m.author}</span>}
      <div className="message-line">
        <div className="message-body">
          {m.file && <FileText size={17} aria-hidden="true" />}
          <span>{m.text}</span>
        </div>
        <div className="message-actions">
          <button
            title="Reply"
            aria-label={`Reply to ${m.author}`}
            onClick={() => demo.setReply(m.text)}
          >
            <ArrowLeft size={14} />
          </button>
          <button
            title={m.reaction ? "Remove reaction" : "React"}
            aria-label={m.reaction ? "Remove reaction" : "React"}
            onClick={() => demo.react(m.id)}
          >
            <Smile size={14} />
          </button>
        </div>
      </div>
      <div className="message-meta">
        <time>{m.time}</time>
        {m.status && (
          <span
            className={`delivery ${m.status}`}
            title={
              m.status === "delivered"
                ? "Delivered to the account, not read"
                : "Waiting to reach the account"
            }
          >
            {m.status === "delivered" ? (
              <CheckCheck size={13} />
            ) : (
              <Clock3 size={13} />
            )}
            {variant !== "a" && (
              <span>
                {m.status === "delivered"
                  ? "Delivered to account"
                  : "Awaiting delivery"}
              </span>
            )}
          </span>
        )}
        {m.reaction && (
          <button
            className="reaction"
            onClick={() => demo.react(m.id)}
            aria-label="Remove thumbs-up reaction"
          >
            {m.reaction} 1
          </button>
        )}
      </div>
    </article>
  );
}

export function Composer({
  demo,
  variant,
}: {
  demo: Demo;
  variant: "a" | "b" | "c";
}) {
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing) return;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      demo.send();
    }
  };
  return (
    <div className="composer-wrap">
      {demo.reply && (
        <div className="reply-banner">
          <span>
            Replying to <strong>{demo.reply}</strong>
          </span>
          <button onClick={() => demo.setReply(null)} aria-label="Cancel reply">
            <X size={15} />
          </button>
        </div>
      )}
      {demo.emojiOpen && (
        <div className="emoji-panel" role="group" aria-label="Emoji">
          {["🙂", "👍", "❤️", "🎉", "👋", "✨", "✅", "🤔"].map((emoji) => (
            <button
              key={emoji}
              onClick={() => {
                demo.setDraft(demo.draft + emoji);
                demo.setEmojiOpen(false);
              }}
            >
              {emoji}
            </button>
          ))}
        </div>
      )}
      <div className="composer">
        <button
          className="compose-attach"
          onClick={() => demo.setOverlay("attach")}
          aria-label="Attach a file"
          title="Attach a file"
        >
          {variant === "a" ? <Plus size={19} /> : <Paperclip size={18} />}
        </button>
        <textarea
          rows={1}
          value={demo.draft}
          onChange={(e) => demo.setDraft(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={`Message ${demo.current.name}`}
          aria-label={`Message ${demo.current.name}`}
        />
        <button
          className="compose-emoji"
          onClick={() => demo.setEmojiOpen(!demo.emojiOpen)}
          aria-label="Choose emoji"
          aria-expanded={demo.emojiOpen}
        >
          <Smile size={18} />
        </button>
        <button
          className="compose-send"
          disabled={!demo.draft.trim()}
          onClick={demo.send}
          aria-label="Send message"
        >
          <Send size={17} />
        </button>
      </div>
      <div className="compose-foot">
        <span>Encrypted conversation</span>
        <span>Enter to send · Shift+Enter for a new line</span>
      </div>
    </div>
  );
}

export function Overlay({ demo }: { demo: Demo }) {
  if (!demo.overlay) return null;
  const title: Record<NonNullable<Demo["overlay"]>, string> = {
    search: "Search messages",
    settings: "Settings",
    connection: "Connection",
    identity: "Your identity",
    files: "Received files",
    history: "Chat history",
    verify: "Verify contact",
    members: "Channel members",
    attach: "Attach a file",
  };
  return (
    <div
      className="overlay-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) demo.setOverlay(null);
      }}
    >
      <section
        className="overlay-panel"
        role="dialog"
        aria-modal="true"
        aria-label={title[demo.overlay]}
      >
        <header>
          <div>
            <span className="eyebrow">MESH-TALK</span>
            <h2>{title[demo.overlay]}</h2>
          </div>
          <button onClick={() => demo.setOverlay(null)} aria-label="Close">
            <X size={19} />
          </button>
        </header>
        {demo.overlay === "search" && (
          <div className="overlay-content">
            <label className="search-field">
              <Search size={17} />
              <input
                autoFocus
                placeholder="Search names and messages"
                value={demo.query}
                onChange={(e) => demo.setQuery(e.target.value)}
              />
            </label>
            <p className="supporting">Select a conversation to open it.</p>
            {demo.visible.map((c) => (
              <button
                className="result-row"
                key={c.id}
                onClick={() => {
                  demo.setSelected(c.id);
                  demo.setOverlay(null);
                }}
              >
                <Avatar person={c} />
                <span>
                  <strong>{c.name}</strong>
                  <small>{c.preview}</small>
                </span>
                <ArrowRight size={16} />
              </button>
            ))}
          </div>
        )}
        {demo.overlay === "settings" && (
          <div className="overlay-content settings-list">
            <div className="setting-row">
              <span>
                <strong>Appearance</strong>
                <small>Choose how Mesh-Talk looks on this device.</small>
              </span>
              <button onClick={() => demo.setCompact(!demo.compact)}>
                {demo.compact ? "Comfortable" : "Compact"}
              </button>
            </div>
            <div className="setting-row">
              <span>
                <strong>Notifications</strong>
                <small>Show alerts when new messages arrive.</small>
              </span>
              <span className="setting-value">On</span>
            </div>
            <div className="setting-row">
              <span>
                <strong>Conversation history</strong>
                <small>Stored locally on this device.</small>
              </span>
              <span className="setting-value">Local</span>
            </div>
            <p className="supporting">
              Prototype controls demonstrate appearance only. No device settings
              are changed.
            </p>
          </div>
        )}
        {demo.overlay === "connection" && (
          <div className="overlay-content">
            <div className="status-hero">
              <Wifi size={20} />
              <span>
                <strong>Ready to find people</strong>
                <small>Connected to Studio Wi-Fi · 2 contacts nearby</small>
              </span>
            </div>
            <div className="detail-row">
              <span>Discovery</span>
              <strong>Searching this local network</strong>
            </div>
            <div className="detail-row">
              <span>Messages</span>
              <strong>Encrypted in transit</strong>
            </div>
            <p className="supporting">
              Contacts appear when their devices are open on the same local
              network.
            </p>
            <button
              className="text-action"
              onClick={() =>
                demo.setNotice("Connection help opened in the prototype.")
              }
            >
              Connection help <ArrowRight size={15} />
            </button>
          </div>
        )}
        {demo.overlay === "identity" && (
          <div className="overlay-content identity-content">
            <span className="identity-glyph">
              <Fingerprint size={34} />
            </span>
            <h3>Alex Morgan</h3>
            <p>Your identity on this device</p>
            <div className="detail-row">
              <span>Account ID</span>
              <code>acc_4f2a…91c0</code>
            </div>
            <div className="detail-row">
              <span>Device fingerprint</span>
              <code>76B9 2A5F 18D4</code>
            </div>
          </div>
        )}
        {demo.overlay === "verify" && (
          <div className="overlay-content">
            <div className="status-hero">
              <ShieldQuestion size={22} />
              <span>
                <strong>
                  {demo.verified
                    ? "Identity verified"
                    : "Identity not yet verified"}
                </strong>
                <small>
                  Compare this safety number with {demo.current.name} through
                  another trusted channel.
                </small>
              </span>
            </div>
            <div className="safety-number">
              4821 7750 1194
              <br />
              6203 8845 2716
            </div>
            <button
              className="primary-action"
              onClick={() => {
                demo.setVerified(!demo.verified);
                demo.setNotice(
                  demo.verified
                    ? "Verification removed in prototype."
                    : "Contact marked verified in prototype.",
                );
              }}
            >
              {demo.verified ? "Remove verification" : "Mark as verified"}
            </button>
            <p className="supporting">
              A verified identity is different from delivery or read status.
            </p>
          </div>
        )}
        {demo.overlay === "members" && (
          <div className="overlay-content">
            <p className="supporting">People in {demo.current.name}.</p>
            {[
              "Alex Morgan",
              "Mira Chen",
              "Owen Patel",
              "Ada Rivera",
              "Nia Brooks",
            ].map((name, i) => (
              <div className="detail-row" key={name}>
                <span>{name}</span>
                <strong>{i === 0 ? "You · owner" : "Member"}</strong>
              </div>
            ))}
          </div>
        )}
        {demo.overlay === "files" && (
          <div className="overlay-content">
            <p className="supporting">
              Attachments shared in your conversations.
            </p>
            <div className="file-row">
              <FileText size={18} />
              <span>
                <strong>Workshop route map.pdf</strong>
                <small>Field team · 2.4 MB</small>
              </span>
              <button
                onClick={() => demo.setNotice("File save action previewed.")}
                aria-label="Save Workshop route map"
              >
                <ArrowDownToLine size={16} />
              </button>
            </div>
            <div className="file-row">
              <FolderOpen size={18} />
              <span>
                <strong>device-list.csv</strong>
                <small>Mira Chen · 18 KB</small>
              </span>
              <button
                onClick={() => demo.setNotice("File save action previewed.")}
                aria-label="Save device list"
              >
                <ArrowDownToLine size={16} />
              </button>
            </div>
          </div>
        )}
        {demo.overlay === "history" && (
          <div className="overlay-content">
            <p className="supporting">
              Messages in your conversation with {demo.current.name}.
            </p>
            {demo.messages.map((m) => (
              <button
                className="history-row"
                key={m.id}
                onClick={() => demo.setOverlay(null)}
              >
                <small>
                  {m.author} · {m.time}
                </small>
                <span>{m.text}</span>
              </button>
            ))}
          </div>
        )}
        {demo.overlay === "attach" && (
          <div className="overlay-content">
            <label className="file-drop">
              <Paperclip size={24} />
              <strong>Choose a file to send</strong>
              <span>It will appear in this conversation.</span>
              <input
                type="file"
                onChange={(e) => demo.chooseFile(e.target.files?.[0])}
              />
            </label>
          </div>
        )}
      </section>
    </div>
  );
}

export function UtilityButton({
  icon,
  label,
  action,
}: {
  icon: React.ReactNode;
  label: string;
  action: () => void;
}) {
  return (
    <button
      className="utility-button"
      title={label}
      aria-label={label}
      onClick={action}
    >
      {icon}
    </button>
  );
}
