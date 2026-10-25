import type {
  FavoriteSource,
  FavoriteMetadata,
  MessageFavorite,
} from "../src/lib/api/messageFavorites";
import type {
  AccountDocumentKey,
  AccountDocumentMeta,
  DocumentVersion,
} from "../src/lib/api/accountDocuments";
import { test as base, expect } from "@playwright/test";
import type {
  ChannelHistoryPolicy,
  PeerInfo,
  PresenceInfo,
} from "../src/lib/types";
import { randomUUID } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import v8ToIstanbul from "v8-to-istanbul";

/** Controls only for retained-attachment scenarios; no product test hooks. */
export interface FavoriteAttachmentMock {
  bytes: number[];
  calls: Array<{ command: string; args: Record<string, unknown> }>;
  saved: Array<{ path: string; bytes: number[] }>;
  authorized: boolean;
  releaseRead?: () => void;
  releaseSaveDialog?: () => void;
}

/**
 * Headless E2E runs the React frontend in plain Chromium — there is no Tauri runtime, so
 * `@tauri-apps/api` (invoke/listen) would throw ("Cannot read properties of undefined
 * (reading 'transformCallback')"). This fixture injects a mock of `window.__TAURI_INTERNALS__`
 * implementing EVERY command the app invokes (see src/lib/api.ts) over a believable, fully
 * deterministic in-memory scenario, so the complete UI flow (register → login → render →
 * open conversation → send → react → search → verify → settings → diagnostics → sign out)
 * runs without the Rust backend.
 *
 * Determinism: state is rebuilt from scratch per page (this fixture runs once per test via
 * `beforeEach`-equivalent injection), so the suite is re-runnable indefinitely without
 * order-dependence or flake. Time is a fixed base so timestamps never drift.
 *
 * `window.__mockEmit(event, payload)` is exposed so a test can simulate an inbound message.
 *
 * The real backend is covered separately by the Rust multi-process suite (`make e2e`); this
 * suite guards the UI rendering + wiring (the layer that black-screened v0.1.0).
 */
export const test = base.extend({
  page: async ({ page }, provide) => {
    // Keep UI scenarios independent of the public catalog. Marketplace tests
    // register their own routes later and override this empty default.
    await page.route("**/marketplace/catalog.json", (route) =>
      route.fulfill({
        contentType: "application/json",
        headers: { "access-control-allow-origin": "*" },
        body: JSON.stringify({ schemaVersion: 1, packs: [] }),
      }),
    );
    await page.addInitScript(() => {
      // E2E-only fixture switch. The URL keeps each adversarial state reloadable without
      // adding test controls to the product UI: ?data=worst|huge|peers-huge|members-huge|empty|offline.
      const dataMode = new URLSearchParams(window.location.search).get("data");
      // --- Identity ---------------------------------------------------------
      const accountProfiles: Record<string, Record<string, unknown>> = {};
      let ownDisplayName = "tester";
      const SELF = { device: "device_self_0001", account: "acc_self_aaaa1111" };
      (window as unknown as Record<string, unknown>).__mockReplaceIdentity = (
        accountId: string,
      ) => {
        SELF.account = accountId;
      };

      // --- Scenario: roster (one account DM + one channel) ------------------
      // Bob: a multi-device account, online, with a known device fingerprint we can verify.
      const BOB = {
        account_id: "acc_bob_bbbb2222",
        device_id: "device_bob_2222",
        name: "bob",
        device_count: 2,
      };
      // Carol: a single-device account, recently-seen (offline-ish), unverified.
      const CAROL = {
        account_id: "acc_carol_cccc3333",
        device_id: "device_carol_3333",
        name: "carol",
        device_count: 1,
      };
      const CHANNEL = {
        channel_id: "chan_team_dddd4444",
        name: "team",
        member_count: 2,
      };

      // The site capture uses the same IPC fixture with a focused, football-themed
      // conversation. Other E2E scenarios keep their ordinary names and messages.
      if (dataMode === "site") {
        BOB.name = "Lionel Messi";
        CAROL.name = "Neymar Jr.";
        CHANNEL.name = "Barcelona × Argentina";
      }

      const peers: PeerInfo[] = [
        {
          user_id: BOB.device_id,
          name: BOB.name,
          addr: "192.168.1.20:47100",
          post_office: false,
          discovered_on_lan: true,
          account_id: BOB.account_id,
        },
        {
          user_id: CAROL.device_id,
          name: CAROL.name,
          addr: "192.168.1.30:47100",
          post_office: false,
          discovered_on_lan: true,
          account_id: CAROL.account_id,
        },
      ];
      if (dataMode === "pairing-routes") {
        peers[0].directly_reachable = true;
        peers[1].directly_reachable = false;
        peers[1].discovered_on_lan = false;
      }
      if (dataMode === "post-office") {
        peers.push({
          user_id: "44444444444444444444444444444444",
          name: "relay node",
          addr: "192.168.1.50:47100",
          post_office: true,
          discovered_on_lan: true,
          account_id: "acc_relay_eeee5555",
        });
        peers.push({
          user_id: "55555555555555555555555555555555",
          name: "remote relay node",
          addr: "192.168.1.50:47477",
          post_office: true,
          discovered_on_lan: false,
          account_id: "acc_relay_ffff6666",
        });
        peers.push({
          user_id: "66666666666666666666666666666666",
          name: "Tailscale relay node",
          addr: "100.101.1.50:47475",
          post_office: true,
          discovered_on_lan: false,
          directly_reachable: true,
          account_id: "acc_relay_aaaa7777",
        });
      }

      (window as unknown as Record<string, unknown>).__mockRotateBobDevice =
        () => {
          peers[0].user_id = "device_bob_rotated_9999";
        };

      (window as unknown as Record<string, unknown>).__mockAddPeer = (
        userId: string,
        accountId: string,
        name: string,
      ) => {
        peers.push({
          user_id: userId,
          account_id: accountId,
          name,
          addr: "192.168.1.40:47100",
          post_office: false,
          discovered_on_lan: true,
        });
        accounts.push({
          account_id: accountId,
          device_count: 1,
          names: [name],
        });
      };

      const accounts = [
        {
          account_id: BOB.account_id,
          device_count: BOB.device_count,
          names: [BOB.name],
        },
        {
          account_id: CAROL.account_id,
          device_count: CAROL.device_count,
          names: [CAROL.name],
        },
      ];

      const channels: Array<{
        channel_id: string;
        name: string;
        member_count: number;
        dissolved?: boolean;
        owner?: string;
        pending_invite?: boolean;
        left?: boolean;
      }> = [
        {
          channel_id: CHANNEL.channel_id,
          name: CHANNEL.name,
          member_count: CHANNEL.member_count,
          dissolved: false,
          owner: dataMode === "invite" ? BOB.device_id : SELF.device,
          pending_invite: dataMode === "invite",
        },
      ];
      const channelHistoryPolicies: Record<string, ChannelHistoryPolicy> = {};
      let channelOwner = dataMode === "invite" ? BOB.device_id : SELF.device;

      (window as unknown as Record<string, unknown>).__mockClearRoster = () => {
        peers.length = 0;
        accounts.length = 0;
        channels.length = 0;
      };

      const channelMembers: Array<{
        user_id: string;
        name: string;
        pending?: boolean;
      }> = [
        // Self is absent from discovery but has explicit runtime presence in the shared snapshot.
        // The raw name exercises the UI's local display-name fallback.
        { user_id: SELF.device, name: SELF.device },
        { user_id: BOB.device_id, name: BOB.name },
        { user_id: CAROL.device_id, name: CAROL.name },
      ];
      if (dataMode === "invite") channelMembers[0].pending = true;

      if (dataMode === "site") channelMembers[0].name = "Cesc Fàbregas";

      if (dataMode === "worst" || dataMode === "huge") {
        const longName = "Aleksandra Wiśniewska-Kowalczyk (Laboratory)";
        BOB.name = longName;
        accounts[0].names = [longName];
        channelMembers[1].name = longName;
        peers[0].name =
          "Aleksandra's development workstation in the shared infrastructure laboratory — " +
          "secondary device used for network diagnostics and release testing";
        CHANNEL.name =
          "Regional infrastructure and security coordination — 東京";
        channels[0].name = CHANNEL.name;

        const names = [
          "Đặng Thị Ngọc Hân",
          "王秀英",
          "👩🏽‍💻 Priya",
          "نور الهدى عبد الرحمن",
          "Jo",
          "Seán O'Brien-Ó Súilleabháin",
          "Christopher Alexander Montgomery III",
          "", // A known account whose announced name is unavailable.
        ];
        names.forEach((name, i) => {
          const account_id = `acc_stress_${i}`;
          const user_id = `device_stress_${i}`;
          accounts.push({
            account_id,
            device_count: i === 0 ? 1284 : 1,
            names: name ? [name] : [],
          });
          peers.push({
            user_id,
            name,
            addr: `192.168.1.${40 + i}:47100`,
            post_office: false,
            discovered_on_lan: true,
            account_id,
          });
        });
      }

      if (dataMode === "huge" || dataMode === "pinned-huge") {
        for (let i = 0; i < 1000; i++) {
          const account_id = `acc_bulk_${i}`;
          accounts.push({
            account_id,
            device_count: 1,
            names: [`Colleague ${String(i + 1).padStart(4, "0")}`],
          });
        }
      }

      if (dataMode === "peers-huge") {
        for (let i = 0; i < 1000; i++) {
          const account_id = `acc_peer_bulk_${i}`;
          accounts.push({
            account_id,
            device_count: 1,
            names: [`Colleague ${String(i + 1).padStart(4, "0")}`],
          });
          peers.push({
            user_id: `device_peer_bulk_${i}`,
            name: `Colleague ${String(i + 1).padStart(4, "0")}`,
            addr: `10.0.${Math.floor(i / 250)}.${(i % 250) + 1}:47100`,
            post_office: false,
            discovered_on_lan: true,
            account_id,
          });
        }
      }

      if (dataMode === "members-huge") {
        for (let i = 0; i < 1000; i++) {
          const account_id = `acc_member_bulk_${i}`;
          const user_id = `device_member_bulk_${i}`;
          const name = `Colleague ${String(i + 1).padStart(4, "0")}`;
          channelMembers.push({ user_id, name });
          peers.push({
            user_id,
            name,
            addr: `10.1.${Math.floor(i / 250)}.${(i % 250) + 1}:47100`,
            post_office: false,
            discovered_on_lan: true,
            account_id,
          });
        }
      }

      if (dataMode === "channels-huge") {
        for (let i = 0; i < 1000; i++) {
          channels.push({
            channel_id: `chan_bulk_${i}`,
            name: `Channel ${String(i + 1).padStart(4, "0")}`,
            member_count: 2,
            dissolved: false,
          });
        }
      }

      if (dataMode === "empty") {
        peers.length = 0;
        accounts.length = 0;
        channels.length = 0;
        channelMembers.length = 0;
      } else if (dataMode === "offline") {
        peers.length = 0;
      }

      // --- Messages (in-memory, per conversation) ---------------------------
      const BASE = 1_700_000_000_000;
      type Msg = {
        protection?: "click" | "authenticate" | null;
        expires_at_ms?: number | null;
        id: string | null;
        from_me: boolean;
        who: string;
        text: string;
        wall_clock: number;
        reply_to: string | null;
        recalled?: boolean;
        recalled_text?: string | null;
        sticker?: string | null;
        file?: {
          name: string;
          size: number;
          mime: string;
          file_conv: string;
          media: boolean;
        };
      };
      let mid = 100;
      const msg = (over: Partial<Msg>): Msg => ({
        id: `m${++mid}`,
        from_me: false,
        who: BOB.device_id,
        text: "",
        wall_clock: BASE + mid * 1000,
        reply_to: null,
        ...over,
      });

      // Keyed by conversation key: `acc:<account_id>` | `ch:<channel_id>` | `dm:<device_id>`.
      const msgs: Record<string, Msg[]> = {
        [`acc:${BOB.account_id}`]: [
          msg({ who: BOB.device_id, text: "hey, welcome to the mesh" }),
          msg({
            from_me: true,
            who: SELF.device,
            text: "thanks! glad to be here",
          }),
        ],
        [`acc:${CAROL.account_id}`]: [],
        [`ch:${CHANNEL.channel_id}`]: [
          msg({ who: BOB.device_id, text: "channel kickoff" }),
        ],
      };

      if (dataMode === "site") {
        msgs[`ch:${CHANNEL.channel_id}`] = [
          msg({
            who: BOB.device_id,
            text: "Training starts at 10:30. Can everyone see the plan from the annex?",
          }),
          msg({
            who: CAROL.device_id,
            text: "Yes. The local network is up, and the plan came through clearly.",
          }),
          msg({
            from_me: true,
            who: SELF.device,
            text: "Perfect. Meet by the south pitch. Bring the blue bibs.",
          }),
          msg({
            who: BOB.device_id,
            text: "I'll bring the match balls. See you there! ⚽",
          }),
        ];
      }

      if (dataMode === "worst" || dataMode === "huge") {
        const samples = [
          "Release notes: " +
            "Network diagnostics and recovery details. ".repeat(55),
          "https://example.com/workspaces/acme/projects/q3-launch/docs/9f8e7d6c5b4a?tab=comments&filter=unresolved",
          "👩🏽‍💻 🦊 🫱🏽‍🫲🏻 🚀 家族とチームの予定を確認してください",
          "Đặng Thị Ngọc Hân — Cập nhật trạng thái kết nối",
          "Line one\nLine two\n────────────────────────────────────────────────────────────────────",
          "<script>alert(1)</script> &amp; **bold**",
        ];
        for (let i = 0; i < 500; i++) {
          msgs[`acc:${BOB.account_id}`].push(
            msg({
              from_me: i % 3 === 0,
              who: i % 3 === 0 ? SELF.device : BOB.device_id,
              text: samples[i % samples.length],
            }),
          );
        }
      }

      // Reactions, keyed the same way; each: { target, emoji, who[] }.
      const reacts: Record<
        string,
        Array<{ target: string; emoji: string; who: string[] }>
      > = {};

      const protectedBodies: Record<string, string> = {};
      const protectedModes: Record<string, string> = {};
      const recordSend = (
        conv: string,
        text: string,
        replyTo: unknown,
        expiresAtMs?: number | null,
      ): null => {
        (msgs[conv] ||= []).push(
          msg({
            from_me: true,
            who: SELF.device,
            text,
            // A real send is "now", so it's inside the recall window (the seeded history
            // uses an old BASE so it isn't recall-eligible).
            wall_clock: Date.now(),
            reply_to: (replyTo as string) ?? null,
            expires_at_ms: expiresAtMs,
          }),
        );
        return null;
      };

      const toggleReact = (
        conv: string,
        target: string,
        emoji: string,
        remove: boolean,
        whoId: string,
      ): null => {
        const arr = (reacts[conv] ||= []);
        const found = arr.find((r) => r.target === target && r.emoji === emoji);
        if (remove) {
          if (found) found.who = found.who.filter((w) => w !== whoId);
          reacts[conv] = arr.filter((r) => r.who.length > 0);
        } else if (found) {
          if (!found.who.includes(whoId)) found.who.push(whoId);
        } else {
          arr.push({ target, emoji, who: [whoId] });
        }
        return null;
      };

      // Message lifecycle: delete removes the line; recall tombstones it (keeps the slot,
      // sets `recalled`); clear empties the conversation.
      const deleteMessage = (conv: string, target: string): null => {
        if (msgs[conv]) msgs[conv] = msgs[conv].filter((m) => m.id !== target);
        return null;
      };
      const recallMessage = (conv: string, target: string): null => {
        const m = msgs[conv]?.find((x) => x.id === target);
        if (m) {
          // Keep the original text re-editable for our own messages (WeChat "re-edit").
          if (m.from_me) m.recalled_text = m.text;
          m.recalled = true;
          m.text = "";
        }
        return null;
      };
      const clearConversation = (conv: string): null => {
        msgs[conv] = [];
        return null;
      };
      const recordSticker = (
        conv: string,
        stickerId: string,
        fallback: string,
      ): null => {
        (msgs[conv] ||= []).push(
          msg({
            from_me: true,
            who: SELF.device,
            text: fallback,
            wall_clock: Date.now(),
            sticker: stickerId,
          }),
        );
        return null;
      };

      // Account-owned message collections, separate from conversation pins.
      const messageCollections: Record<
        string,
        Record<string, MessageFavorite>
      > = {};
      let collectionRevision = 0;

      // Retained bytes deliberately have no source-chat message or ordinary media entry.
      let favoriteAttachment: FavoriteAttachmentMock | undefined;
      if (dataMode?.startsWith("favorites-attachment")) {
        const canvas = document.createElement("canvas");
        canvas.width = 2;
        canvas.height = 2;
        const context = canvas.getContext("2d")!;
        context.fillStyle = "#cc5533";
        context.fillRect(0, 0, 2, 2);
        const bytes = Array.from(atob(canvas.toDataURL().split(",")[1]), (c) =>
          c.charCodeAt(0),
        );
        favoriteAttachment = { bytes, calls: [], saved: [], authorized: true };
        (
          window as unknown as { __favoriteAttachment: FavoriteAttachmentMock }
        ).__favoriteAttachment = favoriteAttachment;
        messageCollections.u_self = {
          retained: {
            id: "retained",
            source: {
              kind: "account",
              target: BOB.account_id,
              message_ids: ["removed-photo"],
            },
            source_label: BOB.name,
            metadata: {
              title: "Retained field photo",
              description: "After source history was cleared",
              notes: { "removed-photo": "Keep for the field report" },
            },
            messages: [
              {
                id: "removed-photo",
                who: BOB.device_id,
                sender_name: BOB.name,
                text: "",
                wall_clock: BASE,
                expires_at_ms: null,
                sticker: null,
                file: {
                  file_conv: "retained-photo",
                  name: "field-photo.png",
                  size: bytes.length,
                  mime: "image/png",
                  media: true,
                },
              },
            ],
            saved_at_ms: BASE,
            revision: ++collectionRevision,
            device_id: SELF.device,
          },
        };
      }
      const retainedFileAuthorized = (a: Record<string, unknown>) =>
        !!favoriteAttachment?.authorized &&
        a.owner === "u_self" &&
        a.fileConv === "retained-photo" &&
        Object.values(messageCollections.u_self ?? {}).some((item) =>
          item.messages.some(
            (message) => message.file?.file_conv === a.fileConv,
          ),
        );

      // --- Favorites (pin + alias), persisted in-memory ---------------------
      const favorites: Record<
        string,
        {
          id: string;
          pinned: boolean;
          custom_alias: string | null;
          hidden?: boolean;
        }
      > = {};
      if (dataMode === "pinned-huge") {
        for (let i = 0; i < 1000; i++) {
          const id = `acc_bulk_${i}`;
          favorites[id] = { id, pinned: true, custom_alias: null };
        }
      }

      // --- Custom avatars (id -> data-URL) ----------------------------------
      const avatars: Record<string, string> = {};
      // Avatars peers PROPAGATED to us, keyed by ACCOUNT id (what `peer_avatars` returns).
      // Bob set a photo → it must show wherever Bob appears, including the channel roster.
      const TINY_PNG =
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
      const receivedAvatars: Record<string, string> = {
        [BOB.account_id]: TINY_PNG,
      };

      if (dataMode === "site") {
        const player = (name: string) =>
          `/site-capture/avatars/players/${encodeURIComponent(name)}.webp`;
        avatars[SELF.account] = player("Cesc Fàbregas");
        avatars[CHANNEL.channel_id] =
          "/site-capture/avatars/clubs/02-barcelona.svg";
        receivedAvatars[BOB.account_id] = player("Lionel Messi");
        receivedAvatars[CAROL.account_id] = player("Neymar");
      }

      // --- Settings ---------------------------------------------------------
      const appSettings = {
        minimize_to_tray: true,
        notifications: true,
        download_dir: "/home/tester/Downloads",
        stay_signed_in: true,
        last_user: null as string | null,
        retention_days: 0,
        calls_enabled: false,
        post_office_enabled: true,
        post_office_send_enabled: true,
        post_office_receive_enabled: true,
        physical_lan_only: false,
        bridge_discovery_enabled: false,
        origin_sharing_enabled: false,
        ringtone: "classic",
        privacy_level: "balanced",
        notification_preview: true,
        auto_lock_seconds: 0,
        lock_when_hidden: false,
        confirm_sensitive_copy: false,
        clipboard_clear_seconds: 0,
      };
      const localAgentAccess = {
        enabled: false,
        running: false,
        destinations: [] as { kind: string; id: string }[],
        available_destinations: [
          {
            destination: { kind: "account", id: BOB.account_id },
            label: BOB.name,
          },
          {
            destination: { kind: "channel", id: CHANNEL.channel_id },
            label: CHANNEL.name,
          },
        ],
        instructions_path: "/home/tester/.codex/AGENTS.md",
        instructions: "",
        error: null,
      };
      let nativeLocked = false;
      let attachmentGlobalAuto = false;
      const attachmentContactAuto: Record<string, boolean> = {};
      const attachmentDecisions: Record<string, boolean> = {};

      // --- Presence (account DM + channel) ----------------------------------
      const presenceMap: Record<string, PresenceInfo> = {
        [BOB.account_id]: {
          online: true,
          last_seen_secs: 2,
          devices: [
            {
              user_id: BOB.device_id,
              evidence: [
                { source: "local", last_seen_secs: 2, expires_in_secs: 28 },
              ],
            },
          ],
        },
        [CAROL.account_id]: { online: false, last_seen_secs: 120, devices: [] },
      };
      let presenceFailure = false;
      (window as unknown as Record<string, unknown>).__mockPresenceFailure = (
        failed: boolean,
      ) => {
        presenceFailure = failed;
      };
      const presenceSnapshot = () => {
        if (presenceFailure)
          throw new Error("Presence temporarily unavailable");
        const snapshot: Record<string, PresenceInfo> = {
          ...presenceMap,
          [SELF.account]: {
            online: true,
            last_seen_secs: 0,
            devices: [
              {
                user_id: SELF.device,
                evidence: [
                  { source: "self", last_seen_secs: 0, expires_in_secs: 30 },
                ],
              },
            ],
          },
        };
        const acceptedAccounts = new Set<string>();
        const unbound = new Set<string>();
        for (const member of channelMembers) {
          const peer = peers.find((p) => p.user_id === member.user_id);
          if (member.pending || peer?.post_office) continue;
          const account =
            member.user_id === SELF.device ? SELF.account : peer?.account_id;
          if (account) acceptedAccounts.add(account);
          else unbound.add(member.user_id);
        }
        snapshot[CHANNEL.channel_id] = {
          online: false,
          last_seen_secs: null,
          devices: [],
          member_keys: [...acceptedAccounts],
          member_count: acceptedAccounts.size,
          online_count: [...acceptedAccounts].filter(
            (id) => snapshot[id]?.online,
          ).length,
          unbound_member_count: unbound.size,
        };
        return snapshot;
      };

      // --- Trust / safety numbers -------------------------------------------
      const verifiedAccounts = new Set<string>();
      const safetyNumber = () => ({
        grouped:
          "12345 67890 12345 67890 12345 67890 12345 67890 12345 67890 12345 67890",
        words: ["anchor", "harbor", "signal", "lantern", "compass", "beacon"],
      });

      // --- Diagnostics ------------------------------------------------------
      const diagPeers = peers.map((p, i) => ({
        user_id: p.user_id,
        name: p.name,
        ip: p.addr.split(":")[0],
        tcp_port: 47100,
        post_office: p.post_office,
        account_id: p.account_id,
        last_seen_secs: i === 0 ? 3 : 90,
      }));

      const networkInfo = {
        own_user_id: SELF.device,
        own_name: "tester",
        account_id: SELF.account,
        listen_tcp_port: 47101,
        discovery_port: 47474,
        multicast_group: "239.255.42.99",
        interfaces: ["192.168.1.10"],
      };
      (
        window as unknown as Record<string, unknown>
      ).__mockSetNetworkInterfaces = (interfaces: string[]) => {
        networkInfo.interfaces = interfaces;
      };

      const envInfo = {
        app_version: "0.1.0",
        data_dir: "/home/tester/.mesh-talk",
        logs_dir: "/home/tester/.mesh-talk/logs",
        os: "linux",
        arch: "x86_64",
        target: "x86_64-unknown-linux-gnu",
        build_profile: "release",
      };

      let contactOwner = "u_self";
      const deletedKey = () => `mock-deleted-${contactOwner}`;
      const deletedContacts = (): string[] =>
        JSON.parse(localStorage.getItem(deletedKey()) ?? "[]");
      const erasedHistory = (): string[] =>
        JSON.parse(localStorage.getItem(`mock-erased-${contactOwner}`) ?? "[]");
      const ok = () => null;
      const probedEndpoints: string[] = [];
      (window as unknown as Record<string, unknown>).__mockProbedEndpoints =
        probedEndpoints;
      let deferredCommand: string | null = null;
      let releaseCommand: (() => void) | null = null;
      let deferredBarrier: Promise<void> | null = null;
      (window as unknown as Record<string, unknown>).__mockDeferNext = (
        command: string,
      ) => {
        deferredCommand = command;
        deferredBarrier = new Promise<void>((resolve) => {
          releaseCommand = resolve;
        });
      };
      (window as unknown as Record<string, unknown>).__mockReleaseDeferred =
        () => releaseCommand?.();
      let failNextCommand: string | null = null;
      let failNextMessage = "network unreachable";
      (window as unknown as Record<string, unknown>).__mockFailNext = (
        command: string,
        message = "network unreachable",
      ) => {
        failNextCommand = command;
        failNextMessage = message;
      };

      // --- Event listener registry (for __mockEmit) -------------------------
      const listeners: Record<
        string,
        Array<{ id: number; cb: (p: unknown) => void }>
      > = {};

      const unregisterListener = (event: string, eventId: number) => {
        listeners[event] = (listeners[event] ?? []).filter(
          (listener) => listener.id !== eventId,
        );
      };

      // Presentation projection control, not a cryptographic receipt oracle.
      // Enqueue is Awaiting only; tests explicitly advance delivery independently.
      const delivery: Record<
        string,
        Record<string, "awaiting" | "delivered">
      > = {};
      const acceptLast = (account: string) => {
        const history = msgs[`acc:${account}`];
        const id = history[history.length - 1].id!;
        if (account !== SELF.account)
          (delivery[account] ||= {})[id] = "awaiting";
        return id;
      };
      (window as unknown as Record<string, unknown>).__mockSetDelivery = (
        account: string,
        id: string,
        status: "awaiting" | "delivered",
      ) => {
        (delivery[account] ||= {})[id] = status;
      };
      // Browser fixture only: account namespaces and CAS mirror the IPC contract;
      // signatures, encrypted persistence and sibling transport are tested natively.
      type MockDocument = { meta: AccountDocumentMeta; payload: string };
      const documentCache = new Map<string, Record<string, MockDocument>>();
      const documentScope = (args: Record<string, unknown>) => {
        if (args.owner !== "u_self" || args.expectedAccount !== SELF.account)
          throw new Error("Account changed");
        return `${args.owner}:${args.expectedAccount}`;
      };
      const documents = (args: Record<string, unknown>) => {
        const scope = documentScope(args);
        let rows = documentCache.get(scope);
        if (!rows) {
          rows = JSON.parse(
            localStorage.getItem(`mock-account-documents-${scope}`) ?? "{}",
          );
          documentCache.set(scope, rows!);
        }
        return rows!;
      };
      const persistDocuments = (args: Record<string, unknown>) => {
        try {
          localStorage.setItem(
            `mock-account-documents-${documentScope(args)}`,
            JSON.stringify(documents(args)),
          );
        } catch {
          /* Large browser-only assets may exceed localStorage quota. */
        }
      };
      const version = (meta: AccountDocumentMeta): DocumentVersion => ({
        revision: meta.revision,
        writer_device: meta.writer_device,
        content_hash: meta.content_hash,
      });
      const matchesVersion = (
        meta: AccountDocumentMeta,
        expected: DocumentVersion | null,
      ) =>
        expected !== null &&
        meta.revision === expected.revision &&
        meta.writer_device === expected.writer_device &&
        JSON.stringify(meta.content_hash) ===
          JSON.stringify(expected.content_hash);
      const writeDocument = (
        args: Record<string, unknown>,
        key: AccountDocumentKey,
        payload: string,
        expected: DocumentVersion | null,
        tombstone = false,
      ) => {
        const rows = documents(args),
          token = JSON.stringify(key),
          old = rows[token];
        if (old ? !matchesVersion(old.meta, expected) : expected !== null)
          throw new Error("Document changed");
        const revision =
          Math.max(0, ...Object.values(rows).map((row) => row.meta.revision)) +
          1;
        const body = tombstone ? "" : payload;
        const meta: AccountDocumentMeta = {
          key,
          revision,
          writer_device: SELF.device,
          content_hash: Array.from({ length: 32 }, (_, index) =>
            tombstone ? 0 : (revision + index) % 256,
          ),
          content_len: atob(body).length,
          tombstone,
        };
        rows[token] = { meta, payload: body };
        persistDocuments(args);
        return structuredClone(meta);
      };
      const snapshotDrafts = (args: Record<string, unknown>) => {
        const drafts: Record<string, string> = {},
          protections: Record<string, string> = {},
          versions: Record<string, DocumentVersion> = {};
        for (const { meta, payload } of Object.values(documents(args))) {
          if (meta.key.kind !== "draft") continue;
          const target = meta.key.conversation,
            key = `${target.kind}:${target.id}`;
          versions[key] = version(meta);
          if (meta.tombstone) continue;
          const value = JSON.parse(
            new TextDecoder().decode(
              Uint8Array.from(atob(payload), (character) =>
                character.charCodeAt(0),
              ),
            ),
          ) as { text: string; protection?: string | null };
          if (value.text) drafts[key] = value.text;
          if (value.protection) protections[key] = value.protection;
        }
        const snapshot = {
          session: `mock-drafts-session:${documentScope(args)}`,
          account_id: SELF.account,
          drafts,
          protections,
          versions,
          conflicts: [] as string[],
        };
        localStorage.setItem(
          `mock-drafts-${documentScope(args)}`,
          JSON.stringify(snapshot),
        );
        return snapshot;
      };
      const draftKey = (key: string): AccountDocumentKey => {
        const [kind, ...parts] = key.split(":");
        if (kind !== "account" && kind !== "channel")
          throw new Error("Invalid draft conversation");
        return { kind: "draft", conversation: { kind, id: parts.join(":") } };
      };
      const encodedDraft = (value: unknown) =>
        btoa(
          Array.from(new TextEncoder().encode(JSON.stringify(value)), (byte) =>
            String.fromCharCode(byte),
          ).join(""),
        );

      const responses: Record<string, (a: Record<string, unknown>) => unknown> =
        {
          owner_node_identity: (a) => ({
            owner: a.owner,
            device_id: SELF.device,
            account_id: SELF.account,
          }),
          owner_reveal_image: (a) => {
            const reference = a.reference as { message_id: string };
            if (!protectedModes[reference.message_id])
              throw new Error("Image unavailable");
            if (
              protectedModes[reference.message_id] === "authenticate" &&
              a.password !== "password123"
            )
              throw new Error("Verification failed");
            return (
              (window as unknown as { __lastWrittenFileBytes?: number[] })
                .__lastWrittenFileBytes ??
              Array.from(atob(TINY_PNG.split(",")[1]), (char) =>
                char.charCodeAt(0),
              )
            );
          },
          owner_reveal_message: (a) => {
            const reference = a.reference as { message_id: string };
            const text = protectedBodies[reference.message_id];
            if (
              protectedModes[reference.message_id] === "authenticate" &&
              a.password !== "password123"
            )
              throw new Error("Verification cancelled or failed");
            if (text === undefined) throw new Error("Message unavailable");
            return { text, visible_for_ms: 30000 };
          },
          owner_account_history: (a) =>
            (msgs[`acc:${a.account}`] ?? []).slice(-Number(a.limit ?? 200)),
          owner_delivery_statuses: (a) =>
            (a.ids as string[]).flatMap((id) => {
              const status = delivery[String(a.account)]?.[id];
              return status ? [{ id, status }] : [];
            }),
          owner_enqueue_channel_text: (a) => {
            recordSend(
              `ch:${a.channelId}`,
              String(a.text),
              a.replyTo,
              a.expiresAtMs as number | null | undefined,
            );
            if (a.protection) {
              const row =
                msgs[`ch:${a.channelId}`][msgs[`ch:${a.channelId}`].length - 1];
              protectedBodies[row.id!] = row.text;
              protectedModes[row.id!] = String(a.protection);
              row.text = "";
              row.protection = a.protection as "click" | "authenticate";
            }
            return msgs[`ch:${a.channelId}`][
              msgs[`ch:${a.channelId}`].length - 1
            ].id;
          },
          owner_enqueue_channel_sticker: (a) => {
            recordSticker(
              `ch:${a.channelId}`,
              String(a.stickerId),
              String(a.fallback),
            );
            return msgs[`ch:${a.channelId}`][
              msgs[`ch:${a.channelId}`].length - 1
            ].id;
          },
          owner_enqueue_channel_file: (a) => {
            const fileConv = `fc_${++mid}`;
            const row = msg({
              protection: a.protection as "click" | "authenticate" | undefined,
              from_me: true,
              who: SELF.device,
              file: {
                name: String(a.path).split(/[\\/]/).pop() || "file",
                size: 123,
                mime: a.protection ? "image/png" : "application/octet-stream",
                file_conv: fileConv,
                media: Boolean(a.media),
              },
            });
            (msgs[`ch:${a.channelId}`] ||= []).push(row);
            if (a.protection) protectedModes[row.id!] = String(a.protection);
            return { id: row.id, fileConv };
          },
          channel_custody_statuses: (a) =>
            (a.ids as string[]).map((id) => ({
              id,
              replicas: 0,
              post_offices: 0,
            })),
          owner_enqueue_text: (a) => {
            recordSend(
              `acc:${a.account}`,
              String(a.text),
              a.replyTo,
              a.expiresAtMs as number | null | undefined,
            );
            if (a.protection) {
              const row =
                msgs[`acc:${a.account}`][msgs[`acc:${a.account}`].length - 1];
              protectedBodies[row.id!] = row.text;
              protectedModes[row.id!] = String(a.protection);
              row.text = "";
              row.protection = a.protection as "click" | "authenticate";
            }
            return acceptLast(String(a.account));
          },
          owner_enqueue_sticker: (a) => {
            recordSticker(
              `acc:${a.account}`,
              String(a.stickerId),
              String(a.fallback),
            );
            return acceptLast(String(a.account));
          },
          owner_enqueue_file: (a) => {
            const fileConv = `fc_${++mid}`;
            (msgs[`acc:${a.account}`] ||= []).push(
              msg({
                protection: a.protection as
                  "click" | "authenticate" | undefined,
                from_me: true,
                who: SELF.device,
                file: {
                  name: String(a.path).split(/[\\/]/).pop() || "file",
                  size: 123,
                  mime: a.protection ? "image/png" : "application/octet-stream",
                  file_conv: fileConv,
                  media: Boolean(a.media),
                },
              }),
            );
            if (a.protection)
              protectedModes[
                msgs[`acc:${a.account}`][
                  msgs[`acc:${a.account}`].length - 1
                ].id!
              ] = String(a.protection);
            return { id: acceptLast(String(a.account)), fileConv };
          },
          // auth
          register: () => ({
            success: true,
            user: { id: "u_self", username: "tester", display_name: "tester" },
          }),
          retry_saved_session: () => "saved",
          login: (a) => {
            contactOwner = a.username === "other_user" ? "u_other" : "u_self";
            for (const account of erasedHistory())
              clearConversation(`acc:${account}`);
            return {
              success: true,
              saved_session: appSettings.stay_signed_in ? "saved" : "disabled",
              user: {
                id: contactOwner,
                username: String(a.username ?? "tester"),
                display_name: String(a.username ?? "tester"),
              },
            };
          },
          rename_account: (a) => {
            ownDisplayName = String(a.newDisplayName ?? "tester");
            if (accountProfiles[SELF.account])
              accountProfiles[SELF.account].display_name = ownDisplayName;
            return {
              id: "u_self",
              username: "tester",
              display_name: String(a.newDisplayName ?? "tester"),
            };
          },
          logout: () => {
            nativeLocked = false;
            return { success: true };
          },
          adopt_linked_account: ok,

          // identity
          my_id: () => SELF.device,
          account_id: () => SELF.account,

          // roster
          list_peers: () => peers.map((peer) => ({ ...peer })),
          list_accounts: () => {
            const deleted = deletedContacts().filter(
              (id) => !presenceMap[id]?.online,
            );
            localStorage.setItem(deletedKey(), JSON.stringify(deleted));
            return accounts
              .filter((account) => !deleted.includes(account.account_id))
              .map((account) => ({
                ...account,
                names: [...account.names],
              }));
          },
          list_channels: () => channels.map((channel) => ({ ...channel })),
          // The test user (SELF) is the channel owner, so the member-management controls
          // (add/remove) stay visible to the existing e2e flow.
          channel_members: (a) => ({
            history_policy:
              channelHistoryPolicies[String(a.channelId)] ??
              "last_ten_within_three_days",
            owner: channelOwner,
            members: channelMembers,
          }),
          channel_activity: () => [],

          // history
          history: (a) => msgs[`dm:${a.peer}`] ?? [],
          account_history: (a) => msgs[`acc:${a.account}`] ?? [],
          channel_history: (a) => {
            const history =
              dataMode === "invite" ? [] : (msgs[`ch:${a.channelId}`] ?? []);
            return dataMode === "group-reception-window"
              ? history.slice(-Number(a.limit ?? 200))
              : history;
          },

          // reactions
          reactions: (a) => reacts[`dm:${a.peer}`] ?? [],
          account_reactions: (a) => reacts[`acc:${a.account}`] ?? [],
          channel_reactions: (a) => reacts[`ch:${a.channelId}`] ?? [],
          react_dm: (a) =>
            toggleReact(
              `dm:${a.recipient}`,
              String(a.target),
              String(a.emoji),
              Boolean(a.remove),
              SELF.account,
            ),
          react_account: (a) =>
            toggleReact(
              `acc:${a.account}`,
              String(a.target),
              String(a.emoji),
              Boolean(a.remove),
              SELF.account,
            ),
          react_channel: (a) =>
            toggleReact(
              `ch:${a.channelId}`,
              String(a.target),
              String(a.emoji),
              Boolean(a.remove),
              SELF.device,
            ),

          // message lifecycle
          delete_message: (a) =>
            deleteMessage(
              a.isChannel ? `ch:${a.convId}` : `acc:${a.convId}`,
              String(a.target),
            ),
          recall_message: (a) =>
            recallMessage(
              a.isChannel ? `ch:${a.convId}` : `acc:${a.convId}`,
              String(a.target),
            ),
          clear_conversation: (a) =>
            clearConversation(
              a.isChannel ? `ch:${a.convId}` : `acc:${a.convId}`,
            ),
          send_sticker: (a) =>
            recordSticker(
              a.isChannel ? `ch:${a.convId}` : `acc:${a.convId}`,
              String(a.stickerId),
              String(a.fallback),
            ),

          // sending
          send_dm: (a) =>
            recordSend(`dm:${a.recipient}`, String(a.text), a.replyTo),
          send_to_account: (a) =>
            recordSend(`acc:${a.account}`, String(a.text), a.replyTo),
          send_channel_message: (a) =>
            recordSend(`ch:${a.channelId}`, String(a.text), a.replyTo),
          send_file_dm: () => "filed",
          send_file_to_account: () => "filed",
          send_file_channel: () => "filed",

          // channels
          create_channel: () => "chan_new_eeee5555",
          set_channel_history_policy: (a) => {
            if (channelOwner !== SELF.device)
              throw new Error(
                "Only the channel owner may change history policy",
              );
            channelHistoryPolicies[String(a.channelId)] =
              a.policy as ChannelHistoryPolicy;
            return null;
          },
          add_channel_member: ok,
          remove_channel_member: ok,
          remove_channel_account_members: ok,
          dissolve_channel: () => {
            channels[0].dissolved = true;
            return null;
          },
          transfer_channel_owner: (a) => {
            channelOwner = String(a.memberId);
            const channel = channels.find((c) => c.channel_id === a.channelId);
            if (channel) channel.owner = channelOwner;
            return null;
          },
          leave_channel: (a) => {
            const channel = channels.find((c) => c.channel_id === a.channelId);
            if (channel) channel.left = true;
            return null;
          },
          accept_channel_invite: (a) => {
            const channel = channels.find((c) => c.channel_id === a.channelId);
            if (channel) channel.pending_invite = false;
            channelMembers[0].pending = false;
            return null;
          },

          // Public profiles contain no private trust history or editable identity metadata.
          get_account_profile: (a) =>
            accountProfiles[String(a.accountId)] ?? {
              account_id: String(a.accountId),
              display_name:
                String(a.accountId) === SELF.account ? ownDisplayName : "Bob",
              bio:
                String(a.accountId) === BOB.account_id ? "Hello from Bob" : "",
              created_at_ms:
                String(a.accountId) === SELF.account ? null : 1700000000000,
            },
          save_account_bio: (a) => {
            accountProfiles[SELF.account] = {
              account_id: SELF.account,
              display_name: ownDisplayName,
              bio: String(a.bio),
              created_at_ms: null,
            };
            return null;
          },
          // trust
          get_trust: (a) => ({
            account_id: String(a.accountId),
            first_seen_fingerprint: String(a.currentFingerprint),
            verified: verifiedAccounts.has(String(a.accountId)),
            fingerprint_changed: dataMode === "changed",
            known: true,
            first_seen_at_ms: 1700000000000,
            device_changes: [],
          }),
          mark_verified: (a) => {
            verifiedAccounts.add(String(a.accountId));
            return null;
          },
          safety_number: () => safetyNumber(),

          // files
          file_statuses: (a) =>
            (a.fileConvs as string[]).map((file_conv) => ({
              file_conv,
              done: 1,
              total: 1,
              ready: true,
              consent:
                attachmentDecisions[file_conv] === undefined
                  ? "accepted"
                  : attachmentDecisions[file_conv]
                    ? "accepted"
                    : "declined",
            })),
          attachment_receive_policy: (a) => ({
            global_auto: attachmentGlobalAuto,
            contact_auto:
              a.account &&
              Object.prototype.hasOwnProperty.call(
                attachmentContactAuto,
                String(a.account),
              )
                ? attachmentContactAuto[String(a.account)]
                : null,
          }),
          set_attachment_global_auto: (a) => {
            attachmentGlobalAuto = Boolean(a.auto);
            return null;
          },
          set_attachment_contact_auto: (a) => {
            if (a.auto === null)
              delete attachmentContactAuto[String(a.account)];
            else attachmentContactAuto[String(a.account)] = Boolean(a.auto);
            return null;
          },
          decide_attachment: (a) => {
            attachmentDecisions[String(a.fileConv)] = Boolean(a.accept);
            return null;
          },
          save_file: ok,
          save_file_to_dir: () => "/home/tester/Downloads/file.bin",
          read_file: () => new ArrayBuffer(0),
          // The durable chat-media store accessor. The mock has no store, so reject like the
          // real "no stored media" path → the preview hook falls back to read_file.
          read_media: () => {
            if (dataMode === "screenshot-ipc-fallback") {
              return (
                (window as unknown as { __lastWrittenFileBytes?: number[] })
                  .__lastWrittenFileBytes ?? []
              );
            }
            throw new Error("no stored media for this file");
          },
          write_temp_file: (a) => {
            if (dataMode === "screenshot-write-fails")
              throw new Error("disk full");
            (
              window as unknown as { __lastWrittenFileBytes?: number[] }
            ).__lastWrittenFileBytes = a.bytes as number[];
            return `/tmp/${String(a.name || "pasted.png")}`;
          },
          delete_temp_file: ok,
          screenshot_available: () => dataMode !== "screenshot-unavailable",
          // A deterministic 320x180 PNG, generated in-browser so the mock always
          // supplies a decodable image for the selection and annotation flow.
          capture_screen: () => {
            const canvas = document.createElement("canvas");
            const desktop = dataMode === "screenshot-desktop";
            canvas.width = desktop ? window.innerWidth * 2 : 320;
            canvas.height = desktop ? window.innerHeight * 2 : 180;
            const context = canvas.getContext("2d");
            if (!context)
              throw new Error("Canvas unavailable in screenshot mock");
            if (dataMode === "screenshot-light") {
              const gradient = context.createLinearGradient(0, 0, 0, 180);
              gradient.addColorStop(0, "#cce3ed");
              gradient.addColorStop(1, "#91b5a6");
              context.fillStyle = gradient;
            } else context.fillStyle = "#1c293d";
            context.fillRect(0, 0, canvas.width, canvas.height);
            if (desktop) {
              context.scale(2, 2);
              context.fillStyle = "#d9e2e7";
              context.fillRect(0, 0, window.innerWidth, window.innerHeight);
              context.fillStyle = "#fff";
              context.fillRect(
                45,
                55,
                window.innerWidth - 90,
                window.innerHeight - 100,
              );
              context.fillStyle = "#eef1f4";
              context.fillRect(45, 55, window.innerWidth - 90, 36);
              context.fillStyle = "#263c48";
              context.font = "16px sans-serif";
              context.fillText("Demo desktop — project notes", 66, 79);
              context.font = "28px sans-serif";
              context.fillText("Screenshot interaction review", 84, 147);
              context.font = "16px sans-serif";
              context.fillText(
                "Select an area. Annotate it. Add it to your message.",
                84,
                183,
              );
              const gradient = context.createLinearGradient(80, 220, 600, 420);
              gradient.addColorStop(0, "#b7d8ce");
              gradient.addColorStop(1, "#618c9d");
              context.fillStyle = gradient;
              context.fillRect(84, 220, window.innerWidth - 180, 180);
              context.fillStyle = "#526777";
              context.fillText(
                "Synthetic fixture · no personal desktop content",
                84,
                window.innerHeight - 80,
              );
            }
            const b64 = canvas.toDataURL("image/png").split(",")[1];
            const bin = atob(b64);
            return Array.from(bin, (char) => char.charCodeAt(0));
          },

          // Search contexts remain separate from the normal history cache.
          owner_search_recent: (a) => ({
            hits: responses.search(a),
            coverage: {
              scope: "recent-window",
              per_conversation_limit: 500,
              result_limit: 200,
              truncated: false,
            },
          }),
          owner_search_context: (a) => {
            const reference = a.reference as {
              kind: string;
              target: string;
              message_id: string | null;
            };
            const key =
              reference.kind === "channel"
                ? `ch:${reference.target}`
                : `acc:${reference.target}`;
            const rows = msgs[key] ?? [];
            const index = rows.findIndex(
              (row) => row.id === reference.message_id,
            );
            const response = {
              status: index >= 0 ? "found" : "unavailable",
              reference,
              messages:
                index >= 0
                  ? rows.slice(Math.max(0, index - 25), index + 26)
                  : [],
              newer_entries: index >= 0 ? rows.length - index - 1 : null,
            };
            if (dataMode === "favorites-delayed-context")
              return new Promise((resolve) => {
                (
                  window as unknown as { __releaseFavoriteContext?: () => void }
                ).__releaseFavoriteContext = () => resolve(response);
              });
            return response;
          },
          // search
          search: (a) => {
            const q = String(a.query ?? "").toLowerCase();
            if (!q) return [];
            const hits: Array<{
              reference: {
                kind: string;
                target: string;
                message_id: string | null;
              };
              expires_at_ms: number | null;
              is_channel: boolean;
              account_id?: string | null;
              target: string;
              label: string;
              from_me: boolean;
              who: string;
              text: string;
              wall_clock: number;
            }> = [];
            for (const [k, list] of Object.entries(msgs)) {
              for (const m of list) {
                if (!m.text.toLowerCase().includes(q)) continue;
                if (k.startsWith("ch:")) {
                  hits.push({
                    reference: {
                      kind: "channel",
                      target: CHANNEL.channel_id,
                      message_id: m.id,
                    },
                    expires_at_ms: m.expires_at_ms ?? null,
                    is_channel: true,
                    target: CHANNEL.channel_id,
                    label: CHANNEL.name,
                    from_me: m.from_me,
                    who: m.who,
                    text: m.text,
                    wall_clock: m.wall_clock,
                  });
                } else {
                  const acct = k.slice(4);
                  const a2 = accounts.find((x) => x.account_id === acct);
                  // device-id target so go() can resolve it to the account
                  const dev = peers.find((p) => p.account_id === acct)?.user_id;
                  hits.push({
                    reference: {
                      kind: "account",
                      target: acct,
                      message_id: m.id,
                    },
                    expires_at_ms: m.expires_at_ms ?? null,
                    is_channel: false,
                    account_id: acct,
                    target: dev ?? acct,
                    label: a2?.names[0] ?? "contact",
                    from_me: m.from_me,
                    who: m.who,
                    text: m.text,
                    wall_clock: m.wall_clock,
                  });
                }
              }
            }
            return hits;
          },

          // device linking
          get_device_sync_policy: () =>
            JSON.parse(
              localStorage.getItem(`mock-device-sync-policy-${SELF.device}`) ??
                '{"history":true,"attachments":false,"profile":true,"appearance":false,"contacts":false}',
            ),
          get_device_sync_status: () => [],
          get_account_login_sync_status: () => ({
            account_id: SELF.account,
            available_revision: null,
            applied_revision: null,
            devices: [],
          }),
          set_device_sync_category: (a) => {
            if (
              ![
                "history",
                "attachments",
                "profile",
                "appearance",
                "contacts",
              ].includes(String(a.category))
            )
              throw new Error("this sync category is not available yet");
            const policy = responses.get_device_sync_policy({}) as Record<
              string,
              boolean
            >;
            const next = {
              ...policy,
              [String(a.category)]: Boolean(a.enabled),
            };
            localStorage.setItem(
              `mock-device-sync-policy-${SELF.device}`,
              JSON.stringify(next),
            );
            return next;
          },
          get_account_preferences: (a) =>
            JSON.parse(
              localStorage.getItem(
                `mock-account-preferences-${SELF.device}-${String(a.category)}`,
              ) ?? "[]",
            ),
          seed_account_preference: (a) => {
            documentScope(a);
            const rows = responses.get_account_preferences(a) as Array<
              Record<string, unknown>
            >;
            return (
              rows.find((row) => row.key === a.key) ??
              responses.set_account_preference(a)
            );
          },
          set_account_preference: (a) => {
            const key = `mock-account-preferences-${SELF.device}-${String(a.category)}`;
            const rows = JSON.parse(localStorage.getItem(key) ?? "[]") as Array<
              Record<string, unknown>
            >;
            const record = {
              category: String(a.category),
              key: String(a.key),
              value: a.value ?? null,
              revision: Date.now(),
              device_id: SELF.device,
            };
            localStorage.setItem(
              key,
              JSON.stringify([
                ...rows.filter((row) => row.key !== record.key),
                record,
              ]),
            );
            return record;
          },
          start_linking: () => "0123456789abcdef0123456789abcdef", // 32 hex, like a real PairingCode
          stop_linking: ok,
          link_device: () => SELF.account,
          rekey_account: () => "acc_rekeyed_ffff6666",

          // Local contact deletion mirrors the native roster/history contract.
          delete_contact: (a) => {
            const account = String(a.account);
            if (
              a.owner !== contactOwner ||
              account === SELF.account ||
              presenceMap[account]?.online
            )
              throw new Error(
                "Only another offline contact can be deleted by the current owner",
              );
            clearConversation(`acc:${account}`);
            localStorage.setItem(
              deletedKey(),
              JSON.stringify([...new Set([...deletedContacts(), account])]),
            );
            localStorage.setItem(
              `mock-erased-${contactOwner}`,
              JSON.stringify([...new Set([...erasedHistory(), account])]),
            );
            const key = `mock-hidden-${contactOwner}`;
            const contacts = (
              JSON.parse(localStorage.getItem(key) ?? "[]") as Array<{
                account_id: string;
                name: string;
              }>
            ).filter((c) => c.account_id !== account);
            localStorage.setItem(key, JSON.stringify(contacts));
            return {
              owner: contactOwner,
              contacts,
              deleted: deletedContacts(),
            };
          },

          // favorites
          get_hidden_contacts: (a) => ({
            owner: String(a.owner),
            contacts: JSON.parse(
              localStorage.getItem(`mock-hidden-${a.owner}`) ?? "[]",
            ),
          }),
          set_contact_hidden: (a) => {
            const key = `mock-hidden-${a.owner}`;
            const contacts: Array<{ account_id: string; name: string }> =
              JSON.parse(localStorage.getItem(key) ?? "[]");
            const next = contacts.filter((c) => c.account_id !== a.account);
            if (a.hidden)
              next.push({
                account_id: String(a.account),
                name: String(a.name),
              });
            localStorage.setItem(key, JSON.stringify(next));
            return { owner: String(a.owner), contacts: next };
          },
          owner_inspect_message_favorites: (a) => {
            const source = a.source as FavoriteSource;
            const items = Object.values(
              messageCollections[String(a.owner)] ?? {},
            ).filter(
              (item) =>
                item.source.kind === source.kind &&
                item.source.target === source.target,
            );
            return {
              existing: structuredClone(
                items.find(
                  (item) =>
                    item.source.message_ids.length ===
                      new Set(source.message_ids).size &&
                    item.source.message_ids.every((id) =>
                      source.message_ids.includes(id),
                    ),
                ) ?? null,
              ),
              saved_message_ids: [
                ...new Set(items.flatMap((item) => item.source.message_ids)),
              ].filter((id) => source.message_ids.includes(id)),
            };
          },
          owner_favorite_file_available: (a) => retainedFileAuthorized(a),
          owner_read_favorite_file: async (a) => {
            if (!retainedFileAuthorized(a))
              throw new Error("Favorite file unavailable");
            const bytes = new Uint8Array(favoriteAttachment!.bytes).buffer;
            if (dataMode === "favorites-attachment-delayed-read")
              await new Promise<void>((resolve) => {
                favoriteAttachment!.releaseRead = resolve;
              });
            // Native read commands return raw IPC bytes, not a JSON array.
            return bytes;
          },
          owner_save_favorite_file: (a) => {
            if (!retainedFileAuthorized(a))
              throw new Error("Favorite file unavailable");
            const path = String(a.dest);
            favoriteAttachment!.saved.push({
              path,
              bytes: [...favoriteAttachment!.bytes],
            });
            return path;
          },
          owner_list_message_favorites: (a) => {
            const q = String(a.query ?? "")
              .trim()
              .toLowerCase();
            const items = Object.values(
              messageCollections[String(a.owner)] ?? {},
            )
              .filter((item) =>
                [
                  item.metadata.title,
                  item.metadata.description,
                  item.source_label,
                  ...Object.values(item.metadata.notes),
                  ...item.messages.flatMap((message) => [
                    message.text,
                    message.sender_name,
                    message.file?.name ?? "",
                  ]),
                ].some((value) => value.toLowerCase().includes(q)),
              )
              .sort((a, b) => b.saved_at_ms - a.saved_at_ms);
            const offset = Number(a.offset ?? 0),
              limit = Number(a.limit ?? 20);
            return {
              items: items.slice(offset, offset + limit),
              has_more: items.length > offset + limit,
            };
          },
          owner_save_message_favorite: (a) => {
            if (dataMode === "favorites-write-fail")
              throw new Error("disk full");
            const source = a.source as FavoriteSource;
            const metadata = a.metadata as FavoriteMetadata;
            const key =
              source.kind === "channel"
                ? `ch:${source.target}`
                : `acc:${source.target}`;
            const selected = (msgs[key] ?? []).filter((message) =>
              source.message_ids.includes(message.id!),
            );
            if (selected.length !== source.message_ids.length)
              throw new Error("Message unavailable");
            const canonical = {
              ...source,
              message_ids: selected.map((message) => message.id!),
            };
            const id = `${source.kind}:${source.target}:${canonical.message_ids.join(":")}`;
            const collection = (messageCollections[String(a.owner)] ??= {});
            if (collection[id]) return structuredClone(collection[id]);
            const item: MessageFavorite = {
              id,
              source: canonical,
              source_label: source.kind === "channel" ? CHANNEL.name : BOB.name,
              metadata: structuredClone(metadata),
              messages: selected.map((message) => ({
                id: message.id!,
                who: message.who,
                sender_name: message.from_me ? "Me" : BOB.name,
                text: message.text,
                wall_clock: message.wall_clock,
                expires_at_ms: message.expires_at_ms ?? null,
                sticker: message.sticker ?? null,
                file: message.file ?? null,
              })),
              saved_at_ms: Date.now(),
              revision: ++collectionRevision,
              device_id: SELF.device,
            };
            collection[id] = item;
            return structuredClone(item);
          },
          owner_edit_message_favorite: (a) => {
            const item = messageCollections[String(a.owner)]?.[String(a.id)];
            if (
              !item ||
              item.revision !== a.expectedRevision ||
              item.device_id !== a.expectedDeviceId
            )
              throw new Error("Favorite was modified; refresh and retry");
            item.metadata = structuredClone(a.metadata as FavoriteMetadata);
            item.revision = ++collectionRevision;
            return structuredClone(item);
          },
          owner_remove_message_favorite: (a) => {
            const collection = messageCollections[String(a.owner)] ?? {};
            const item = collection[String(a.id)];
            if (
              !item ||
              item.revision !== a.expectedRevision ||
              item.device_id !== a.expectedDeviceId
            )
              throw new Error("Favorite was modified; refresh and retry");
            delete collection[item.id];
            return null;
          },
          get_favorites: () => Object.values(favorites),
          set_favorite: (a) => {
            const id = String(a.id);
            const prev = favorites[id];
            favorites[id] = {
              id,
              pinned: Boolean(a.pinned),
              custom_alias: prev?.custom_alias ?? null,
              hidden: prev?.hidden ?? false,
            };
            if (
              !favorites[id].pinned &&
              !favorites[id].custom_alias &&
              !favorites[id].hidden
            )
              delete favorites[id];
            return null;
          },
          set_alias: (a) => {
            const id = String(a.id);
            const prev = favorites[id];
            const alias =
              a.alias == null ? null : String(a.alias).trim() || null;
            favorites[id] = {
              id,
              pinned: prev?.pinned ?? false,
              custom_alias: alias,
              hidden: prev?.hidden ?? false,
            };
            if (
              !favorites[id].pinned &&
              !favorites[id].custom_alias &&
              !favorites[id].hidden
            )
              delete favorites[id];
            return null;
          },
          set_channel_hidden: (a) => {
            const id = String(a.id);
            const prev = favorites[id];
            favorites[id] = {
              id,
              pinned: prev?.pinned ?? false,
              custom_alias: prev?.custom_alias ?? null,
              hidden: Boolean(a.hidden),
            };
            return null;
          },

          // avatars (custom profile photos)
          get_avatars: () => avatars,
          set_avatar: (a) => {
            const id = String(a.id);
            if (a.data_url == null) delete avatars[id];
            else avatars[id] = String(a.data_url);
            return null;
          },
          // avatars peers propagated to us (none in the mock); publishing is a no-op
          peer_avatars: () => receivedAvatars,
          publish_avatar: (args) => {
            documentScope(args);
            return null;
          },

          // persistent login: no saved session in the mock → show the login screen
          auto_login: () => null,
          lock_app: () => {
            nativeLocked = true;
            return null;
          },
          note_activity: ok,
          set_call_active: ok,
          lock_status: () =>
            nativeLocked
              ? { id: "u_self", username: "tester", display_name: "tester" }
              : null,
          unlock_app: (a) => {
            if (a.password && a.password !== "password123")
              throw new Error("Invalid password");
            nativeLocked = false;
            return null;
          },
          clear_saved_session: ok,
          media_migration_warning: () => false,

          // Synthetic recovery fixture; never use this key outside browser tests.
          prepare_backup_key: () =>
            "11111111-22222222-33333333-44444444-55555555-66666666-77777777-88888888",
          backup_status: () => null,

          // settings
          notification_permission: () => ({
            status: "granted",
            platform: "macos",
            can_open_settings: true,
          }),
          request_notification_permission: () => ({
            status: "granted",
            platform: "macos",
            can_open_settings: true,
          }),
          open_notification_settings: () => null,
          get_local_agent_access: () => localAgentAccess,
          set_local_agent_access: (a) => {
            const destinations = a.destinations as {
              kind: string;
              id: string;
            }[];
            if (a.enabled && destinations.length === 0)
              throw new Error("Select a conversation");
            Object.assign(localAgentAccess, {
              enabled: a.enabled,
              running: a.enabled,
              destinations,
              instructions: a.enabled
                ? 'Mesh Talk local API: "http://127.0.0.1:12345/v1/help".\nRead this help URL before using the API for user-authorized tasks.\nThe desktop app must be running and unlocked, with agent access enabled.\n'
                : "",
            });
            return localAgentAccess;
          },
          get_app_settings: () => appSettings,
          reconcile_account_settings: () => appSettings,
          load_conversation_drafts: snapshotDrafts,
          save_conversation_drafts: (a) => {
            if (a.session !== `mock-drafts-session:${documentScope(a)}`)
              throw new Error("Draft writer retired");
            const changes = a.changes as Record<
              string,
              {
                text: string;
                protection?: string;
                expected: DocumentVersion | null;
              }
            >;
            const conflicts: string[] = [];
            for (const [key, change] of Object.entries(changes)) {
              try {
                writeDocument(
                  a,
                  draftKey(key),
                  encodedDraft({
                    text: change.text,
                    protection: change.protection,
                  }),
                  change.expected,
                  !change.text && !change.protection,
                );
              } catch {
                conflicts.push(key);
              }
            }
            return { ...snapshotDrafts(a), conflicts };
          },
          publish_legacy_account_controls: (a) => {
            documentScope(a);
            // This fixture has no historical reaction or recall controls to import.
            return null;
          },
          publish_legacy_conversation_drafts: (a) => {
            documentScope(a);
            const legacy = JSON.parse(
              localStorage.getItem(`mock-drafts-${a.owner}`) ?? "{}",
            );
            const texts = (legacy.drafts ?? legacy) as Record<string, string>,
              protections = (legacy.protections ?? {}) as Record<
                string,
                string
              >;
            let imported = 0,
              unmapped = 0;
            for (const key of new Set([
              ...Object.keys(texts),
              ...Object.keys(protections),
            ])) {
              const documentKey = draftKey(key);
              if (documents(a)[JSON.stringify(documentKey)]) continue;
              if (documentKey.kind !== "draft") continue;
              const target = documentKey.conversation;
              if (
                target.kind === "account"
                  ? ![SELF.account, BOB.account_id, CAROL.account_id].includes(
                      target.id,
                    )
                  : target.id !== CHANNEL.channel_id
              ) {
                unmapped++;
                continue;
              }
              writeDocument(
                a,
                documentKey,
                encodedDraft({
                  text: texts[key] ?? "",
                  protection: protections[key],
                }),
                null,
              );
              imported++;
            }
            return { imported, unmapped };
          },
          get_account_documents: (a) => {
            const items = Object.values(documents(a))
              .map((row) => row.meta)
              .filter((meta) => meta.key.kind === a.domain)
              .sort((left, right) =>
                JSON.stringify(left.key).localeCompare(
                  JSON.stringify(right.key),
                ),
              );
            const after = a.after ? JSON.stringify(a.after) : null;
            const remaining = after
              ? items.filter(
                  (meta) => JSON.stringify(meta.key).localeCompare(after) > 0,
                )
              : items;
            const limit = Math.min(64, Math.max(1, Number(a.limit) || 64));
            const selected = remaining.slice(0, limit);
            return {
              items: structuredClone(selected),
              next:
                remaining.length > limit
                  ? selected[selected.length - 1].key
                  : null,
              digest: Array.from(
                { length: 32 },
                (_, index) => (items.length + index) % 256,
              ),
            };
          },
          get_account_document_meta: (a) =>
            structuredClone(documents(a)[JSON.stringify(a.key)]?.meta ?? null),
          read_account_document: (a) => {
            const row = documents(a)[JSON.stringify(a.key)];
            if (
              !row ||
              row.meta.tombstone ||
              !matchesVersion(row.meta, a.expected as DocumentVersion)
            )
              throw new Error("Document changed");
            return Array.from(atob(row.payload), (character) =>
              character.charCodeAt(0),
            );
          },
          put_account_document: (a) =>
            writeDocument(
              a,
              a.key as AccountDocumentKey,
              String(a.payloadBase64),
              a.expected as DocumentVersion | null,
            ),
          remove_account_document: (a) =>
            writeDocument(
              a,
              a.key as AccountDocumentKey,
              "",
              a.expected as DocumentVersion | null,
              true,
            ),
          mark_account_message_seen: (a) => {
            const key: AccountDocumentKey = {
              kind: "read_receipt",
              conversation: a.conversation as Extract<
                AccountDocumentKey,
                { kind: "draft" }
              >["conversation"],
              message_id: String(a.messageId),
            };
            return structuredClone(
              documents(a)[JSON.stringify(key)]?.meta ??
                writeDocument(a, key, "", null),
            );
          },
          set_app_settings: (a) => {
            Object.assign(appSettings, (a.settings as object) ?? {});
            if (appSettings.privacy_level === "balanced") {
              Object.assign(appSettings, {
                notification_preview: true,
                auto_lock_seconds: 0,
                lock_when_hidden: false,
                confirm_sensitive_copy: false,
                clipboard_clear_seconds: 0,
              });
            } else if (appSettings.privacy_level === "private") {
              Object.assign(appSettings, {
                notification_preview: false,
                auto_lock_seconds: 600,
                lock_when_hidden: false,
                confirm_sensitive_copy: false,
                clipboard_clear_seconds: 60,
              });
            } else if (appSettings.privacy_level === "strict") {
              Object.assign(appSettings, {
                notification_preview: false,
                auto_lock_seconds: 60,
                lock_when_hidden: true,
                confirm_sensitive_copy: true,
                clipboard_clear_seconds: 30,
              });
            }
            return null;
          },

          // Privacy has dedicated canonical-account scenarios in privacy.spec.ts.
          // Legacy roster fixtures use synthetic IDs, so keep this default empty.
          get_account_binding_changes: (a) => ({
            owner: String(a.owner),
            changes: [],
          }),
          accept_account_binding_change: (a) => ({
            owner: String(a.owner),
            version: 1,
            invisible: false,
            allowed_accounts: [],
          }),
          get_privacy: (a) => ({
            owner: String(a.owner),
            version: 1,
            invisible: false,
            allowed_accounts: [],
          }),
          initiate_privacy_contact: (a) => ({
            owner: String(a.owner),
            version: 1,
            invisible: false,
            allowed_accounts: [],
          }),
          set_invisible: (a) => ({
            owner: String(a.owner),
            version: 1,
            invisible: Boolean(a.invisible),
            allowed_accounts: [],
          }),
          set_privacy_allowed: (a) => ({
            owner: String(a.owner),
            version: 1,
            invisible: false,
            allowed_accounts: [],
          }),

          // presence
          get_presence: presenceSnapshot,

          // diagnostics / observability
          diag_get_peers: () => diagPeers,
          diag_network_info: () => networkInfo,
          rescan_peers: ok,
          probe_peer_endpoint: (a) => {
            probedEndpoints.push(String(a.host));
            return null;
          },
          set_badge: ok,
          network_name: () => "TestNet-5G",
          env_info: () => envInfo,
          get_logs_dir: () => envInfo.logs_dir,
          get_log_file: () => `${envInfo.logs_dir}/mesh-talk.log`,
          read_log_tail: () => "INFO node started\nINFO discovery announced",
          save_log_tail: ok,

          // autostart plugin (invoked as plugin commands; mocked as no-ops below)
        };

      (window as unknown as Record<string, unknown>).__mockSetAccountBio = (
        accountId: string,
        bio: string,
      ) => {
        accountProfiles[accountId] = {
          account_id: accountId,
          display_name: accountId === SELF.account ? ownDisplayName : "Bob",
          bio,
          created_at_ms: accountId === SELF.account ? null : 1700000000000,
        };
      };
      // Expose deterministic event emission for tests that simulate inbound traffic.
      (window as unknown as Record<string, unknown>).__mockEmit = (
        event: string,
        payload: unknown,
      ) => {
        for (const listener of listeners[event] ?? []) listener.cb(payload);
      };

      (window as unknown as Record<string, unknown>).__mockSetPresence = (
        next: Record<string, PresenceInfo>,
      ) => {
        Object.assign(presenceMap, next);
      };

      // Inject an inbound message into the store (so a subsequent history reload returns it),
      // mirroring how the real backend persists a received message before emitting its event.
      (window as unknown as Record<string, unknown>).__mockInject = (
        conv: string,
        text: string,
        who: string,
        metadata?: {
          id?: string;
          index?: number;
          wall_clock?: number;
          expires_at_ms?: number;
        },
      ) => {
        const entry = msg({ from_me: false, who, text, ...metadata });
        const history = (msgs[conv] ||= []);
        if (metadata?.index !== undefined)
          history.splice(metadata.index, 0, entry);
        else history.push(entry);
        return entry.id;
      };

      Object.defineProperty(window, "__TAURI_INTERNALS__", {
        value: {
          // getCurrentWindow()/getCurrentWebview() read these labels.
          metadata: {
            currentWindow: { label: "main" },
            currentWebview: { label: "main" },
          },
          transformCallback(cb: (p: unknown) => void) {
            const id = Math.floor(Math.random() * 1e9);
            (window as unknown as Record<string, unknown>)[`_${id}`] = cb;
            return id;
          },
          async invoke(cmd: string, args: Record<string, unknown>) {
            if (cmd === deferredCommand) {
              deferredCommand = null;
              await deferredBarrier;
            }
            if (
              favoriteAttachment &&
              /^(owner_.*favorite|read_media$|read_file$|save_file|plugin:dialog\|save$)/.test(
                cmd,
              )
            )
              favoriteAttachment.calls.push({
                command: cmd,
                args: structuredClone(args ?? {}),
              });
            if (favoriteAttachment && cmd === "plugin:dialog|save") {
              if (dataMode === "favorites-attachment-delayed-dialog")
                await new Promise<void>((resolve) => {
                  favoriteAttachment!.releaseSaveDialog = resolve;
                });
              return "/home/tester/Downloads/field-photo.png";
            }
            if (cmd === failNextCommand) {
              failNextCommand = null;
              throw new Error(failNextMessage);
            }
            // Tauri event plugin: register the JS callback against the event name so
            // __mockEmit can deliver a payload to it (mirrors `listen`).
            if (cmd === "plugin:event|listen") {
              const ev = String(args.event);
              const handlerId = args.handler as number;
              const cb = (window as unknown as Record<string, unknown>)[
                `_${handlerId}`
              ] as ((m: { payload: unknown }) => void) | undefined;
              const eventId = Math.floor(Math.random() * 1e9);
              (listeners[ev] ||= []).push({
                id: eventId,
                cb: (payload) => cb?.({ payload }),
              });
              return eventId;
            }
            if (cmd === "plugin:event|unlisten") {
              unregisterListener(String(args.event), Number(args.eventId));
              return null;
            }
            // Autostart plugin commands (read/enable/disable) — benign defaults.
            if (cmd.startsWith("plugin:autostart|")) {
              return cmd.endsWith("is_enabled") ? false : null;
            }
            // Other plugin commands (dialog/opener/shell/notification) — no-op.
            if (cmd.startsWith("plugin:")) return null;
            const fn = responses[cmd];
            return fn ? fn(args || {}) : null;
          },
          convertFileSrc: (p: string) => p,
        },
        configurable: true,
      });

      Object.defineProperty(window, "__TAURI_EVENT_PLUGIN_INTERNALS__", {
        value: { unregisterListener },
        configurable: true,
      });
    });
    const collectCoverage = process.env.MESH_TALK_E2E_COVERAGE === "1";
    if (collectCoverage)
      await page.coverage.startJSCoverage({ resetOnNavigation: false });
    try {
      await provide(page);
    } finally {
      if (collectCoverage) {
        const files: Record<string, number[]> = {};
        for (const entry of await page.coverage.stopJSCoverage()) {
          if (!entry.source || !/\/src\/[^?]+\.tsx?(?:\?|$)/.test(entry.url))
            continue;
          const converter = v8ToIstanbul(entry.url, 0, {
            source: entry.source,
          });
          await converter.load();
          converter.applyCoverage(entry.functions);
          for (const [url, coverage] of Object.entries(
            converter.toIstanbul(),
          )) {
            const lines = new Set(files[url] ?? []);
            for (const [id, statement] of Object.entries(
              coverage.statementMap,
            )) {
              if (coverage.s[id] > 0) lines.add(statement.start.line);
            }
            files[url] = [...lines];
          }
        }
        const directory = resolve("coverage/e2e-parts");
        await mkdir(directory, { recursive: true });
        await writeFile(
          resolve(directory, `${randomUUID()}.json`),
          JSON.stringify(files),
        );
      }
    }
  },
});

export { expect };
