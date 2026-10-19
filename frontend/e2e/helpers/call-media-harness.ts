import { expect, type Page } from "@playwright/test";

export type Harness = {
  phase: () => string;
  signal: (from: string, payload: string) => void;
  start: (target: string) => Promise<void>;
  accept: () => Promise<void>;
  hangup: () => void;
  stats: () => Promise<Record<string, number | boolean | string>>;
  cleaned: () => boolean;
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
) {
  await page.route("**/__call-media", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: `<!doctype html><title>Call media verification</title><script type="module">
      import { useCalls } from '/src/store/calls.ts';
      import { useAuth } from '/src/store/auth.ts';
      import { useSettings } from '/src/store/settings.ts';
      const connections = [], tracks = [];
      const remoteVideo = document.createElement('video');
      remoteVideo.autoplay = true; remoteVideo.muted = true; document.body.append(remoteVideo);
      const NativeConnection = window.RTCPeerConnection;
      window.RTCPeerConnection = class extends NativeConnection {
        constructor(options) { super(options); connections.push(this); }
        get localDescription() {
          const description = super.localDescription;
          if (!description || !${JSON.stringify(tailscaleOnly)}) return description;
          // Test-only path constraint: expose only Tailscale candidates to the
          // remote peer, then independently inspect the selected native pair.
          const sdp = description.sdp.split('\\r\\n').filter(line =>
            !line.startsWith('a=candidate:') || line.split(' ')[4]?.startsWith('100.')
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
      useCalls.subscribe(state => {
        if (remoteVideo.srcObject !== state.remoteStream) remoteVideo.srcObject = state.remoteStream;
        for (const track of state.localStream?.getTracks() ?? []) {
          if (!tracks.includes(track)) tracks.push(track);
        }
      });
      window.__callMediaHarness = {
        phase: () => useCalls.getState().phase,
        signal: (from, payload) => useCalls.getState().onSignal({ from, payload }),
        start: target => useCalls.getState().startCall({ name: target, accountId: target, deviceIds: [target] }, true),
        accept: () => useCalls.getState().accept(),
        hangup: () => useCalls.getState().hangup(),
        stats: async () => {
          const result = { audioSent: 0, audioReceived: 0, videoSent: 0, videoReceived: 0, dtls: false, videoFrames: 0, audioSamples: 0, localAddress: '', remoteAddress: '' };
          const all = await connections[0].getStats();
          for (const report of all.values()) {
            if (report.type === 'transport' && report.selectedCandidatePairId) {
              const pair = all.get(report.selectedCandidatePairId);
              result.localAddress = all.get(pair.localCandidateId)?.address ?? '';
              result.remoteAddress = all.get(pair.remoteCandidateId)?.address ?? '';
            }
            if (report.type === 'inbound-rtp') {
              if (report.kind === 'video') result.videoFrames += report.framesDecoded ?? 0;
              if (report.kind === 'audio') result.audioSamples += report.totalSamplesReceived ?? 0;
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
        cleaned: () => tracks.length === 2 && tracks.every(track => track.readyState === 'ended') &&
          connections.every(connection => connection.connectionState === 'closed') &&
          useCalls.getState().localStream === null && useCalls.getState().remoteStream === null,
      };
    </script>`,
    }),
  );
  await page.goto(`${url}/__call-media`);
  await expect
    .poll(() => page.evaluate(() => Boolean(window.__callMediaHarness)))
    .toBe(true);
}
