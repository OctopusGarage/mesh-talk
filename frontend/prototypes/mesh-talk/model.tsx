import { useState } from "react";

export type Conversation = {
  id: string;
  name: string;
  kind: "person" | "channel";
  presence: string;
  preview: string;
  time: string;
  unread?: number;
  verified?: boolean;
  initials: string;
};

export type Message = {
  id: number;
  author: string;
  text: string;
  time: string;
  mine?: boolean;
  status?: "delivered" | "awaiting" | "failed";
  reaction?: string;
  file?: boolean;
};

export const conversations: Conversation[] = [
  {
    id: "mira",
    name: "Mira Chen",
    kind: "person",
    presence: "Online · nearby",
    preview: "The design notes are ready to review.",
    time: "10:42",
    unread: 2,
    verified: true,
    initials: "MC",
  },
  {
    id: "field",
    name: "Field team",
    kind: "channel",
    presence: "5 members",
    preview: "Owen: I've shared the route map.",
    time: "09:18",
    unread: 1,
    initials: "FT",
  },
  {
    id: "owen",
    name: "Owen Patel",
    kind: "person",
    presence: "Last seen 12 min ago",
    preview: "Thanks, that makes sense.",
    time: "Yesterday",
    initials: "OP",
  },
  {
    id: "ada",
    name: "Ada Rivera",
    kind: "person",
    presence: "Offline",
    preview: "See you at the workshop.",
    time: "Tuesday",
    verified: true,
    initials: "AR",
  },
];

const seed: Record<string, Message[]> = {
  mira: [
    {
      id: 1,
      author: "Mira",
      text: "Morning. Are we still meeting at the workshop at eleven?",
      time: "10:31",
    },
    {
      id: 2,
      author: "You",
      text: "Yes. I’ll bring the updated device list.",
      time: "10:33",
      mine: true,
      status: "delivered",
    },
    {
      id: 3,
      author: "Mira",
      text: "Perfect. I want to check the pairing flow on both machines.",
      time: "10:38",
      reaction: "👍",
    },
    {
      id: 4,
      author: "Mira",
      text: "The design notes are ready to review.",
      time: "10:42",
    },
    {
      id: 5,
      author: "You",
      text: "I’ll take a look before we meet.",
      time: "10:44",
      mine: true,
      status: "awaiting",
    },
  ],
  field: [
    {
      id: 11,
      author: "Owen",
      text: "I've shared the route map for tomorrow.",
      time: "09:18",
      file: true,
    },
    {
      id: 12,
      author: "Mira",
      text: "Got it. I’ll check the meeting points.",
      time: "09:21",
    },
  ],
  owen: [
    {
      id: 21,
      author: "Owen",
      text: "Thanks, that makes sense.",
      time: "Yesterday",
    },
  ],
  ada: [
    {
      id: 31,
      author: "Ada",
      text: "See you at the workshop.",
      time: "Tuesday",
    },
  ],
};

export function useDemo() {
  const [selected, setSelected] = useState("mira");
  const [messages, setMessages] = useState(seed);
  const [draft, setDraft] = useState("");
  const [query, setQuery] = useState("");
  const [overlay, setOverlay] = useState<
    | "search"
    | "settings"
    | "connection"
    | "identity"
    | "files"
    | "history"
    | "verify"
    | "members"
    | "attach"
    | null
  >(null);
  const [notice, setNotice] = useState("");
  const [emojiOpen, setEmojiOpen] = useState(false);
  const [reply, setReply] = useState<string | null>(null);
  const [verification, setVerification] = useState<Record<string, boolean>>({
    mira: true,
    ada: true,
  });
  const [compact, setCompact] = useState(false);
  const current = conversations.find((c) => c.id === selected)!;
  const verified = !!verification[selected];
  const setVerified = (value: boolean) =>
    setVerification((all) => ({ ...all, [selected]: value }));
  const visible = conversations.filter((c) =>
    `${c.name} ${c.preview} ${messages[c.id].map((m) => m.text).join(" ")}`
      .toLowerCase()
      .includes(query.toLowerCase()),
  );
  const send = () => {
    const text = draft.trim();
    if (!text) return;
    const next = {
      id: Date.now(),
      author: "You",
      text: reply ? `↳ ${reply}\n${text}` : text,
      time: new Date().toLocaleTimeString([], {
        hour: "2-digit",
        minute: "2-digit",
      }),
      mine: true,
      status: current.kind === "person" ? ("awaiting" as const) : undefined,
    };
    setMessages((all) => ({ ...all, [selected]: [...all[selected], next] }));
    setDraft("");
    setReply(null);
    setNotice("Message queued for delivery to the contact’s account.");
  };
  const react = (id: number) =>
    setMessages((all) => ({
      ...all,
      [selected]: all[selected].map((m) =>
        m.id === id ? { ...m, reaction: m.reaction ? undefined : "👍" } : m,
      ),
    }));
  const chooseFile = (file: File | undefined) => {
    if (!file) return;
    setMessages((all) => ({
      ...all,
      [selected]: [
        ...all[selected],
        {
          id: Date.now(),
          author: "You",
          text: file.name,
          time: new Date().toLocaleTimeString([], {
            hour: "2-digit",
            minute: "2-digit",
          }),
          mine: true,
          file: true,
          status: current.kind === "person" ? ("awaiting" as const) : undefined,
        },
      ],
    }));
    setOverlay(null);
    setNotice("Attachment queued for delivery.");
  };
  return {
    selected,
    setSelected,
    messages: messages[selected],
    draft,
    setDraft,
    query,
    setQuery,
    overlay,
    setOverlay,
    notice,
    setNotice,
    emojiOpen,
    setEmojiOpen,
    reply,
    setReply,
    verified,
    setVerified,
    compact,
    setCompact,
    current,
    visible,
    send,
    react,
    chooseFile,
  };
}

export type Demo = ReturnType<typeof useDemo>;
