import { expect, type Page } from "@playwright/test";

export type Harness = {
  phase: () => string;
  signal: (from: string, payload: string) => void;
  start: (target: string, video?: boolean) => Promise<void>;
  accept: () => Promise<void>;
  hangup: () => void;
  stats: () => Promise<Record<string, number | boolean | string>>;
  cleaned: () => boolean;
  playback: () => { playing: boolean; videoTracks: number; players: number };
};
declare global {
  interface Window {
    __callMediaHarness: Harness;
  }
}

export async function prepareCallMedia(
  page: Page,
  url: string,
  id: string,
  tailscaleOnly = false,
  cameraUnavailable = false,
  candidateAddresses?: readonly string[],
) {
  await page.route("**/__call-media", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: `<!doctype html><title>Call media verification</title><script type="module">
      import RefreshRuntime from '/@react-refresh';
      RefreshRuntime.injectIntoGlobalHook(window);
      window.$RefreshReg$ = () => {};
      window.$RefreshSig$ = () => type => type;
      window.__vite_plugin_react_preamble_installed__ = true;
      const { mountCallDialog } = await import('/e2e/call-dialog-harness.tsx');
      import { useCalls } from '/src/store/calls.ts';
      import { useAuth } from '/src/store/auth.ts';
      import { useSettings } from '/src/store/settings.ts';
      const connections = [], tracks = [];
      if (${JSON.stringify(cameraUnavailable)}) {
        const capture = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
        navigator.mediaDevices.getUserMedia = constraints => constraints.video
          ? Promise.reject(new DOMException('No test camera', 'NotFoundError'))
          : capture(constraints);
      }
      const NativeConnection = window.RTCPeerConnection;
      window.RTCPeerConnection = class extends NativeConnection {
        constructor(options) { super(options); connections.push(this); }
        get localDescription() {
          const description = super.localDescription;
          const addresses = ${JSON.stringify(candidateAddresses ?? [])};
          if (!description || (!${JSON.stringify(tailscaleOnly)} && !addresses.length)) return description;
          // Test-only path constraint: expose only Tailscale candidates to the
          // remote peer, then independently inspect the selected native pair.
          const sdp = description.sdp.split('\\r\\n').filter(line =>
            !line.startsWith('a=candidate:') || (addresses.length
              ? addresses.includes(line.split(' ')[4])
              : line.split(' ')[4]?.startsWith('100.'))
          ).join('\\r\\n');
          return new RTCSessionDescription({ type: description.type, sdp });
        }
      };
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'send_call_signal') return window.__deliverCallSignal(args);
        if (command === 'initiate_privacy_contact') return null;
        throw new Error('Unexpected IPC: ' + command);
      }};
      useAuth.setState({ user: { id: ${JSON.stringify(id)}, display_name: ${JSON.stringify(id)} } });
      useSettings.setState({ callsEnabled: true });
      mountCallDialog();
      useCalls.subscribe(state => {
        for (const track of state.localStream?.getTracks() ?? []) {
          if (!tracks.includes(track)) tracks.push(track);
        }
      });
      window.__callMediaHarness = {
        phase: () => useCalls.getState().phase,
        signal: (from, payload) => useCalls.getState().onSignal({ from, payload }),
        start: (target, video = true) => useCalls.getState().startCall({ name: target, accountId: target, deviceIds: [target] }, video),
        accept: () => useCalls.getState().accept(),
        hangup: () => useCalls.getState().hangup(),
        stats: async () => {
          const result = { audioSent: 0, audioReceived: 0, videoSent: 0, videoReceived: 0, dtls: false, videoFrames: 0, audioSamples: 0, audioEnergy: 0, localAddress: '', remoteAddress: '' };
          const all = await connections[0].getStats();
          for (const report of all.values()) {
            if (report.type === 'transport' && report.selectedCandidatePairId) {
              const pair = all.get(report.selectedCandidatePairId);
              result.localAddress = all.get(pair.localCandidateId)?.address ?? '';
              result.remoteAddress = all.get(pair.remoteCandidateId)?.address ?? '';
            }
            if (report.type === 'inbound-rtp') {
              if (report.kind === 'video') result.videoFrames += report.framesDecoded ?? 0;
              if (report.kind === 'audio') {
                result.audioSamples += report.totalSamplesReceived ?? 0;
                result.audioEnergy += report.totalAudioEnergy ?? 0;
              }
            }
          }
          for (const entry of (await connections[0].getStats()).values()) {
            if (entry.type === 'transport' && entry.dtlsState === 'connected') result.dtls = true;
            if (entry.kind === 'audio' || entry.kind === 'video') {
              if (entry.type === 'outbound-rtp') result[entry.kind + 'Sent'] += entry.bytesSent ?? 0;
              if (entry.type === 'inbound-rtp') result[entry.kind + 'Received'] += entry.bytesReceived ?? 0;
            }
          }
          return result;
        },
        playback: () => {
          const stream = useCalls.getState().remoteStream;
          const players = [...document.querySelectorAll('video,audio')].filter(player => stream && player.srcObject === stream);
          return {
            playing: players.length === 1 && !players[0].paused && !players[0].muted && players[0].currentTime > 0,
            videoTracks: stream?.getVideoTracks().length ?? 0,
            players: players.length,
          };
        },
        cleaned: () => tracks.length > 0 && tracks.every(track => track.readyState === 'ended') &&
          connections.every(connection => connection.connectionState === 'closed') &&
          useCalls.getState().localStream === null && useCalls.getState().remoteStream === null &&
          [...document.querySelectorAll('video,audio')].every(player => !player.srcObject),
      };
    </script>`,
    }),
  );
  await page.goto(`${url}/__call-media`);
  // The shipped UI loads a cold module graph over the physical fixture's SSH
  // tunnel too. Match chat-shell startup's budget, separately from call setup.
  await expect
    .poll(() => page.evaluate(() => Boolean(window.__callMediaHarness)), {
      timeout: 15_000,
    })
    .toBe(true);
}
