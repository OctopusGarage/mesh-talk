import {
  FolderOpen,
  History,
  Search,
  Settings2,
  ShieldCheck,
  ShieldQuestion,
} from "lucide-react";
import type { Demo } from "./model";
import {
  Avatar,
  Composer,
  ConversationItems,
  Mark,
  MessageList,
  UtilityButton,
} from "./parts";

export function Native({ demo }: { demo: Demo }) {
  return (
    <div className={`app native ${demo.compact ? "compact" : ""}`}>
      <aside className="native-sidebar">
        <div className="native-sidebar-head">
          <div className="window-dots">
            <i />
            <i />
            <i />
          </div>
          <div className="native-title">
            <Mark small />
            <strong>Mesh-Talk</strong>
            <button
              aria-label="Search messages"
              onClick={() => demo.setOverlay("search")}
            >
              <Search size={17} />
            </button>
          </div>
          <label className="native-search">
            <Search size={15} />
            <input
              placeholder="Find a conversation"
              value={demo.query}
              onChange={(e) => demo.setQuery(e.target.value)}
            />
          </label>
        </div>
        <div className="native-list">
          <div className="section-heading">
            CONVERSATIONS <span>4</span>
          </div>
          <ConversationItems demo={demo} variant="a" />
        </div>
        <div className="native-bottom">
          <button onClick={() => demo.setOverlay("connection")}>
            <span className="status-dot" />
            Ready to find people <span className="network-count">2 nearby</span>
          </button>
          <div className="native-bottom-actions">
            <button onClick={() => demo.setOverlay("files")}>
              <FolderOpen size={16} /> Files
            </button>
            <button onClick={() => demo.setOverlay("settings")}>
              <Settings2 size={16} /> Settings
            </button>
          </div>
          <button
            className="self-row"
            onClick={() => demo.setOverlay("identity")}
          >
            <span className="self-avatar">AM</span>
            <span>
              <strong>Alex Morgan</strong>
              <small>Your identity</small>
            </span>
          </button>
        </div>
      </aside>
      <main className="native-main">
        <header className="native-header">
          <Avatar person={demo.current} />
          <div className="header-identity">
            <strong>{demo.current.name}</strong>
            <span>{demo.current.presence}</span>
          </div>
          <div className="header-actions">
            <UtilityButton
              icon={<History size={18} />}
              label="Chat history"
              action={() => demo.setOverlay("history")}
            />
            <button
              className="native-trust"
              onClick={() =>
                demo.setOverlay(
                  demo.current.kind === "channel" ? "members" : "verify",
                )
              }
            >
              {demo.current.kind === "channel" ? (
                <span>5</span>
              ) : demo.verified ? (
                <ShieldCheck size={17} />
              ) : (
                <ShieldQuestion size={17} />
              )}
              <span>
                {demo.current.kind === "channel"
                  ? "Members"
                  : demo.verified
                    ? "Verified"
                    : "Verify"}
              </span>
            </button>
          </div>
        </header>
        <MessageList demo={demo} variant="a" />
        <Composer demo={demo} variant="a" />
      </main>
    </div>
  );
}
