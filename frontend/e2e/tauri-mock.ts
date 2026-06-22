import { test as base, expect } from "@playwright/test";

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
    await page.addInitScript(() => {
      // --- Identity ---------------------------------------------------------
      const SELF = { device: "device_self_0001", account: "acc_self_aaaa1111" };

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

      const peers = [
        {
          user_id: BOB.device_id,
          name: BOB.name,
          addr: "192.168.1.20:47100",
          post_office: false,
          account_id: BOB.account_id,
        },
        {
          user_id: CAROL.device_id,
          name: CAROL.name,
          addr: "192.168.1.30:47100",
          post_office: false,
          account_id: CAROL.account_id,
        },
      ];

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

      const channels = [
        {
          channel_id: CHANNEL.channel_id,
          name: CHANNEL.name,
          member_count: CHANNEL.member_count,
        },
      ];

      const channelMembers = [
        // Self is a member of its own channel. We never appear in our own discovery
        // roster, so the backend can only fall back to our raw user_id for the name and
        // there is no presence entry for us — the UI must fill in our name + online state.
        { user_id: SELF.device, name: SELF.device },
        { user_id: BOB.device_id, name: BOB.name },
        { user_id: CAROL.device_id, name: CAROL.name },
      ];

      // --- Messages (in-memory, per conversation) ---------------------------
      const BASE = 1_700_000_000_000;
      type Msg = {
        id: string | null;
        from_me: boolean;
        who: string;
        text: string;
        wall_clock: number;
        reply_to: string | null;
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

      // Reactions, keyed the same way; each: { target, emoji, who[] }.
      const reacts: Record<
        string,
        Array<{ target: string; emoji: string; who: string[] }>
      > = {};

      const recordSend = (
        conv: string,
        text: string,
        replyTo: unknown,
      ): null => {
        (msgs[conv] ||= []).push(
          msg({
            from_me: true,
            who: SELF.device,
            text,
            reply_to: (replyTo as string) ?? null,
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

      // --- Favorites (pin + alias), persisted in-memory ---------------------
      const favorites: Record<
        string,
        { id: string; pinned: boolean; custom_alias: string | null }
      > = {};

      // --- Custom avatars (id -> data-URL) ----------------------------------
      const avatars: Record<string, string> = {};
      // Avatars peers PROPAGATED to us, keyed by ACCOUNT id (what `peer_avatars` returns).
      // Bob set a photo → it must show wherever Bob appears, including the channel roster.
      const TINY_PNG =
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
      const receivedAvatars: Record<string, string> = {
        [BOB.account_id]: TINY_PNG,
      };

      // --- Settings ---------------------------------------------------------
      const appSettings = {
        minimize_to_tray: true,
        notifications: true,
        download_dir: "/home/tester/Downloads",
        stay_signed_in: true,
        last_user: null as string | null,
      };

      // --- Presence (account DM + channel) ----------------------------------
      const presenceMap: Record<
        string,
        { online: boolean; last_seen_secs: number | null }
      > = {
        [BOB.account_id]: { online: true, last_seen_secs: 2 },
        [CAROL.account_id]: { online: false, last_seen_secs: 120 },
        [CHANNEL.channel_id]: { online: true, last_seen_secs: 5 },
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
        post_office: false,
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

      const envInfo = {
        app_version: "0.1.0",
        data_dir: "/home/tester/.mesh-talk",
        logs_dir: "/home/tester/.mesh-talk/logs",
        os: "linux",
        arch: "x86_64",
        target: "x86_64-unknown-linux-gnu",
        build_profile: "release",
      };

      const ok = () => null;

      // --- Event listener registry (for __mockEmit) -------------------------
      const listeners: Record<string, Array<(p: unknown) => void>> = {};

      const responses: Record<string, (a: Record<string, unknown>) => unknown> =
        {
          // auth
          register: () => ({
            success: true,
            user: { id: "u_self", username: "tester" },
          }),
          login: (a) => ({
            success: true,
            user: { id: "u_self", username: String(a.username ?? "tester") },
          }),
          logout: () => ({ success: true }),
          adopt_linked_account: ok,

          // identity
          my_id: () => SELF.device,
          account_id: () => SELF.account,

          // roster
          list_peers: () => peers,
          list_accounts: () => accounts,
          list_channels: () => channels,
          // The test user (SELF) is the channel owner, so the member-management controls
          // (add/remove) stay visible to the existing e2e flow.
          channel_members: () => ({
            owner: SELF.device,
            members: channelMembers,
          }),

          // history
          history: (a) => msgs[`dm:${a.peer}`] ?? [],
          account_history: (a) => msgs[`acc:${a.account}`] ?? [],
          channel_history: (a) => msgs[`ch:${a.channelId}`] ?? [],

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
          add_channel_member: ok,
          remove_channel_member: ok,

          // trust
          get_trust: (a) => ({
            account_id: String(a.accountId),
            first_seen_fingerprint: String(a.currentFingerprint),
            verified: verifiedAccounts.has(String(a.accountId)),
            fingerprint_changed: false,
            known: true,
          }),
          mark_verified: (a) => {
            verifiedAccounts.add(String(a.accountId));
            return null;
          },
          safety_number: () => safetyNumber(),

          // files
          save_file: ok,
          save_file_to_dir: () => "/home/tester/Downloads/file.bin",
          read_file: () => new ArrayBuffer(0),
          // The durable chat-media store accessor. The mock has no store, so reject like the
          // real "no stored media" path → the preview hook falls back to read_file.
          read_media: () => {
            throw new Error("no stored media for this file");
          },
          write_temp_file: () => "/tmp/pasted.png",
          // A deterministic 1x1 PNG (decoded from a fixed base64). Returned as a number[]
          // (the byte array the real command yields) so the screenshot send path flows.
          capture_screen: () => {
            const b64 =
              "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
            const bin = atob(b64);
            const out: number[] = [];
            for (let i = 0; i < bin.length; i++) out.push(bin.charCodeAt(i));
            return out;
          },

          // search
          search: (a) => {
            const q = String(a.query ?? "").toLowerCase();
            if (!q) return [];
            const hits: Array<{
              is_channel: boolean;
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
                    is_channel: false,
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
          start_linking: () => "123456",
          stop_linking: ok,
          link_device: () => SELF.account,
          rekey_account: () => "acc_rekeyed_ffff6666",

          // favorites
          get_favorites: () => Object.values(favorites),
          set_favorite: (a) => {
            const id = String(a.id);
            const prev = favorites[id];
            favorites[id] = {
              id,
              pinned: Boolean(a.pinned),
              custom_alias: prev?.custom_alias ?? null,
            };
            if (!favorites[id].pinned && !favorites[id].custom_alias)
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
            };
            if (!favorites[id].pinned && !favorites[id].custom_alias)
              delete favorites[id];
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
          publish_avatar: ok,

          // persistent login: no saved session in the mock → show the login screen
          auto_login: () => null,
          clear_saved_session: ok,

          // settings
          get_app_settings: () => appSettings,
          set_app_settings: (a) => {
            Object.assign(appSettings, (a.settings as object) ?? {});
            return null;
          },

          // presence
          get_presence: () => presenceMap,

          // diagnostics / observability
          diag_get_peers: () => diagPeers,
          diag_network_info: () => networkInfo,
          rescan_peers: ok,
          set_badge: ok,
          env_info: () => envInfo,
          get_logs_dir: () => envInfo.logs_dir,
          get_log_file: () => `${envInfo.logs_dir}/mesh-talk.log`,
          read_log_tail: () => "INFO node started\nINFO discovery announced",
          save_log_tail: ok,

          // autostart plugin (invoked as plugin commands; mocked as no-ops below)
        };

      // Expose deterministic event emission for tests that simulate inbound traffic.
      (window as unknown as Record<string, unknown>).__mockEmit = (
        event: string,
        payload: unknown,
      ) => {
        for (const cb of listeners[event] ?? []) cb(payload);
      };

      // Inject an inbound message into the store (so a subsequent history reload returns it),
      // mirroring how the real backend persists a received message before emitting its event.
      (window as unknown as Record<string, unknown>).__mockInject = (
        conv: string,
        text: string,
        who: string,
      ) => {
        (msgs[conv] ||= []).push(msg({ from_me: false, who, text }));
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
            // Tauri event plugin: register the JS callback against the event name so
            // __mockEmit can deliver a payload to it (mirrors `listen`).
            if (cmd === "plugin:event|listen") {
              const ev = String(args.event);
              const handlerId = args.handler as number;
              const cb = (window as unknown as Record<string, unknown>)[
                `_${handlerId}`
              ] as ((m: { payload: unknown }) => void) | undefined;
              (listeners[ev] ||= []).push((payload) => cb?.({ payload }));
              return Math.floor(Math.random() * 1e9);
            }
            if (cmd === "plugin:event|unlisten") return null;
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
    });
    await provide(page);
  },
});

export { expect };
