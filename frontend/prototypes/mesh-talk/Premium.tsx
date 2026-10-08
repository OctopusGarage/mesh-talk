import {
  ArrowUpRight,
  History,
  Search,
  Settings2,
  ShieldCheck,
  ShieldQuestion,
  Wifi,
} from "lucide-react";
import type { Demo } from "./model";
import {
  Avatar,
  Composer,
  ConversationItems,
  Mark,
  MessageList,
} from "./parts";

export function Premium({ demo }: { demo: Demo }) {
  return (
    <div className={`app premium ${demo.compact ? "compact" : ""}`}>
      <header className="premium-top">
        <div className="premium-brand">
          <Mark small />
          <span>Mesh-Talk</span>
        </div>
        <nav aria-label="Main">
          <button className="selected" onClick={() => demo.setOverlay(null)}>
            Messages
          </button>
          <button onClick={() => demo.setOverlay("files")}>Files</button>
          <button onClick={() => demo.setOverlay("connection")}>
            Connection
          </button>
        </nav>
        <div className="premium-top-right">
          <span>
            <span className="status-dot" /> 2 people nearby
          </span>
          <button
            onClick={() => demo.setOverlay("settings")}
            aria-label="Settings"
          >
            <Settings2 size={17} />
          </button>
          <button
            className="top-self"
            onClick={() => demo.setOverlay("identity")}
            aria-label="Your identity"
          >
            AM
          </button>
        </div>
      </header>
      <div className="premium-body">
        <aside className="premium-sidebar">
          <div className="premium-sidebar-head">
            <span className="eyebrow">YOUR SPACE</span>
            <h1>
              Messages<span className="title-count">04</span>
            </h1>
            <button
              onClick={() => demo.setOverlay("search")}
              aria-label="Search messages"
            >
              <Search size={19} />
            </button>
          </div>
          <div className="premium-sidebar-sub">
            The people and conversations on your network.
          </div>
          <ConversationItems demo={demo} variant="c" />
          <div className="premium-sidebar-foot">
            <button onClick={() => demo.setOverlay("connection")}>
              <Wifi size={16} />
              <span>Ready to find people</span>
              <ArrowUpRight size={15} />
            </button>
          </div>
        </aside>
        <main className="premium-main">
          <div className="premium-conversation">
            <header className="premium-header">
              <div className="premium-header-person">
                <Avatar person={demo.current} size="large" />
                <div>
                  <span className="eyebrow">
                    {demo.current.kind === "channel"
                      ? "GROUP CONVERSATION"
                      : "PRIVATE CONVERSATION"}
                  </span>
                  <h2>{demo.current.name}</h2>
                  <p>{demo.current.presence}</p>
                </div>
              </div>
              <div className="premium-header-actions">
                <button onClick={() => demo.setOverlay("history")}>
                  <History size={17} /> History
                </button>
                <button
                  onClick={() =>
                    demo.setOverlay(
                      demo.current.kind === "channel" ? "members" : "verify",
                    )
                  }
                >
                  {demo.current.kind === "channel" ? (
                    "5 members"
                  ) : demo.verified ? (
                    <ShieldCheck size={17} />
                  ) : (
                    <ShieldQuestion size={17} />
                  )}{" "}
                  {demo.current.kind === "channel"
                    ? "Members"
                    : demo.verified
                      ? "Verified"
                      : "Verify identity"}
                </button>
              </div>
            </header>
            <div className="premium-encryption">
              <span className="premium-rule" />
              <span>Messages are end-to-end encrypted</span>
              <span className="premium-rule" />
            </div>
            <MessageList demo={demo} variant="c" />
            <Composer demo={demo} variant="c" />
          </div>
        </main>
      </div>
    </div>
  );
}
