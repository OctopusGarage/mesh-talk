import {
  Activity,
  FileText,
  Fingerprint,
  History,
  MessageSquareText,
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
  UtilityButton,
} from "./parts";

export function Technical({ demo }: { demo: Demo }) {
  return (
    <div className={`app technical ${demo.compact ? "compact" : ""}`}>
      <nav className="tech-rail" aria-label="Workspace">
        <div className="tech-rail-top">
          <Mark />
          <button
            className="active"
            title="Conversations"
            aria-label="Conversations"
            onClick={() => demo.setOverlay(null)}
          >
            <MessageSquareText size={20} />
          </button>
          <button
            title="Search"
            aria-label="Search"
            onClick={() => demo.setOverlay("search")}
          >
            <Search size={20} />
          </button>
          <button
            title="Received files"
            aria-label="Received files"
            onClick={() => demo.setOverlay("files")}
          >
            <FileText size={20} />
          </button>
          <button
            title="Connection"
            aria-label="Connection"
            onClick={() => demo.setOverlay("connection")}
          >
            <Activity size={20} />
          </button>
        </div>
        <div className="tech-rail-bottom">
          <button
            title="Settings"
            aria-label="Settings"
            onClick={() => demo.setOverlay("settings")}
          >
            <Settings2 size={20} />
          </button>
          <button
            className="rail-avatar"
            title="Your identity"
            aria-label="Your identity"
            onClick={() => demo.setOverlay("identity")}
          >
            AM
          </button>
        </div>
      </nav>
      <aside className="tech-sidebar">
        <header>
          <span className="eyebrow">WORKSPACE / 01</span>
          <h1>Conversations</h1>
          <p>Local network · 2 contacts nearby</p>
        </header>
        <label className="tech-filter">
          <Search size={15} />
          <input
            placeholder="Filter conversations"
            value={demo.query}
            onChange={(e) => demo.setQuery(e.target.value)}
          />
        </label>
        <div className="tech-group-title">
          <span>DIRECT & CHANNELS</span>
          <span>04</span>
        </div>
        <ConversationItems demo={demo} variant="b" />
        <div className="tech-sidebar-foot">
          <span className="pulse" /> Discovery active <span>STUDIO WI-FI</span>
        </div>
      </aside>
      <main className="tech-main">
        <header className="tech-header">
          <div>
            <span className="eyebrow">
              SECURE CONVERSATION /{" "}
              {demo.current.kind === "channel" ? "CHANNEL" : "DIRECT"}
            </span>
            <div className="tech-name-row">
              <h2>{demo.current.name}</h2>
              <span className="tech-presence">{demo.current.presence}</span>
            </div>
          </div>
          <div className="tech-header-actions">
            <UtilityButton
              icon={<History size={18} />}
              label="Chat history"
              action={() => demo.setOverlay("history")}
            />
            <button
              onClick={() =>
                demo.setOverlay(
                  demo.current.kind === "channel" ? "members" : "verify",
                )
              }
            >
              <ShieldQuestion size={16} />{" "}
              {demo.current.kind === "channel" ? "Members" : "Trust details"}
            </button>
          </div>
        </header>
        <div className="tech-security-strip">
          <ShieldCheck size={15} />
          <span>Encrypted conversation</span>
          <span className="strip-sep">/</span>
          <span>
            {demo.current.kind === "channel"
              ? "5 members"
              : demo.verified
                ? "Identity verified"
                : "Identity not yet verified"}
          </span>
          <button
            onClick={() =>
              demo.setOverlay(
                demo.current.kind === "channel" ? "members" : "verify",
              )
            }
          >
            Review <span>↗</span>
          </button>
        </div>
        <MessageList demo={demo} variant="b" />
        <Composer demo={demo} variant="b" />
      </main>
      <aside className="tech-inspector">
        <span className="eyebrow">
          {demo.current.kind === "channel" ? "CHANNEL" : "CONTACT"} /{" "}
          {demo.current.id.toUpperCase()}
        </span>
        <div className="inspector-identity">
          <Avatar person={demo.current} size="large" />
          <h3>{demo.current.name}</h3>
          <p>{demo.current.presence}</p>
        </div>
        <div className="inspector-block">
          <span>{demo.current.kind === "channel" ? "PEOPLE" : "TRUST"}</span>
          <button
            onClick={() =>
              demo.setOverlay(
                demo.current.kind === "channel" ? "members" : "verify",
              )
            }
          >
            <Fingerprint size={17} />
            <span>
              {demo.current.kind === "channel"
                ? "View channel members"
                : demo.verified
                  ? "Verified identity"
                  : "Compare safety number"}
            </span>{" "}
            ↗
          </button>
        </div>
        <div className="inspector-block">
          <span>DELIVERY</span>
          <p>
            Account receipts confirm arrival. They do not show whether a message
            was read.
          </p>
        </div>
        <div className="inspector-block">
          <span>NETWORK</span>
          <p>
            <Wifi size={15} /> Studio Wi-Fi
          </p>
          <button onClick={() => demo.setOverlay("connection")}>
            Connection details ↗
          </button>
        </div>
      </aside>
    </div>
  );
}
