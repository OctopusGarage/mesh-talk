/**
 * Call ringtones, synthesized with the Web Audio API — no bundled audio (works offline, no
 * codec or copyrighted-tune licensing). A small curated set of classics; the user picks one
 * in Settings. The chosen tone plays for INCOMING calls; the caller always hears the standard
 * ringback while the other end rings.
 */

export type RingtoneId = "classic" | "oldPhone" | "digital" | "marimba";
export const RINGTONE_IDS: RingtoneId[] = [
  "classic",
  "oldPhone",
  "digital",
  "marimba",
];
export const DEFAULT_RINGTONE: RingtoneId = "classic";

export function asRingtoneId(s: string | undefined): RingtoneId {
  return (RINGTONE_IDS as string[]).includes(s ?? "")
    ? (s as RingtoneId)
    : DEFAULT_RINGTONE;
}

interface Ring {
  /** Milliseconds between ring cycles. */
  periodMs: number;
  /** Schedule ONE cycle starting at ctx-time `t`, onto `dest`. */
  render: (ctx: AudioContext, dest: AudioNode, t: number) => void;
}

// --- tone primitives ---

/** The classic dual-tone phone bell: two close sines whose beat (|f2 − f1| Hz) IS the iconic
 * "brrring" warble, band-limited to the telephone band, with click-free attack/release. */
function burst(
  ctx: AudioContext,
  dest: AudioNode,
  f1: number,
  f2: number,
  at: number,
  dur: number,
) {
  const o1 = ctx.createOscillator();
  const o2 = ctx.createOscillator();
  const lp = ctx.createBiquadFilter();
  const env = ctx.createGain();
  o1.type = "sine";
  o2.type = "sine";
  o1.frequency.value = f1;
  o2.frequency.value = f2;
  lp.type = "lowpass";
  lp.frequency.value = 2000;
  o1.connect(env);
  o2.connect(env);
  env.connect(lp);
  lp.connect(dest);
  env.gain.setValueAtTime(0.0001, at);
  env.gain.exponentialRampToValueAtTime(1, at + 0.02);
  env.gain.setValueAtTime(1, at + dur - 0.04);
  env.gain.exponentialRampToValueAtTime(0.0001, at + dur);
  o1.start(at);
  o2.start(at);
  o1.stop(at + dur + 0.02);
  o2.stop(at + dur + 0.02);
}

/** A bell/marimba note: sine + quiet octave partial with a fast-attack / exponential-decay
 * pluck envelope (the decay is what makes it a chime, not a beep). */
function pluck(
  ctx: AudioContext,
  dest: AudioNode,
  freq: number,
  at: number,
  dur: number,
  level = 1,
) {
  const osc = ctx.createOscillator();
  const partial = ctx.createOscillator();
  const env = ctx.createGain();
  const partialGain = ctx.createGain();
  osc.type = "sine";
  partial.type = "sine";
  osc.frequency.value = freq;
  partial.frequency.value = freq * 2;
  partialGain.gain.value = 0.22;
  osc.connect(env);
  partial.connect(partialGain);
  partialGain.connect(env);
  env.connect(dest);
  env.gain.setValueAtTime(0.0001, at);
  env.gain.exponentialRampToValueAtTime(level, at + 0.012);
  env.gain.exponentialRampToValueAtTime(0.0001, at + dur);
  osc.start(at);
  partial.start(at);
  osc.stop(at + dur + 0.05);
  partial.stop(at + dur + 0.05);
}

/** A short retro feature-phone bleep (square through a mild low-pass). */
function blip(
  ctx: AudioContext,
  dest: AudioNode,
  freq: number,
  at: number,
  dur: number,
) {
  const osc = ctx.createOscillator();
  const lp = ctx.createBiquadFilter();
  const env = ctx.createGain();
  osc.type = "square";
  osc.frequency.value = freq;
  lp.type = "lowpass";
  lp.frequency.value = 3500;
  osc.connect(env);
  env.connect(lp);
  lp.connect(dest);
  env.gain.setValueAtTime(0.0001, at);
  env.gain.exponentialRampToValueAtTime(0.6, at + 0.005);
  env.gain.setValueAtTime(0.6, at + dur - 0.02);
  env.gain.exponentialRampToValueAtTime(0.0001, at + dur);
  osc.start(at);
  osc.stop(at + dur + 0.02);
}

// --- the ringtones ---

const RINGTONES: Record<RingtoneId, Ring> = {
  // UK-style "ring–ring": two short bursts then a gap.
  classic: {
    periodMs: 3000,
    render: (ctx, d, t) => {
      burst(ctx, d, 400, 450, t, 0.4);
      burst(ctx, d, 400, 450, t + 0.6, 0.4);
    },
  },
  // North-American single long ring (440 + 480 Hz, the network ring tones).
  oldPhone: {
    periodMs: 3000,
    render: (ctx, d, t) => burst(ctx, d, 440, 480, t, 1.2),
  },
  // Retro feature-phone: four quick alternating bleeps.
  digital: {
    periodMs: 2200,
    render: (ctx, d, t) =>
      [988, 1319, 988, 1319].forEach((f, i) =>
        blip(ctx, d, f, t + i * 0.12, 0.09),
      ),
  },
  // Warm marimba chime: a bright ascending arpeggio.
  marimba: {
    periodMs: 2400,
    render: (ctx, d, t) =>
      [659.25, 830.61, 987.77, 1318.51].forEach((f, i) =>
        pluck(ctx, d, f, t + i * 0.13, 0.5),
      ),
  },
};

/** What the caller hears while the callee's phone rings — the standard ringback, regardless
 * of the callee's chosen ringtone (which they never hear). */
const RINGBACK: Ring = {
  periodMs: 4000,
  render: (ctx, d, t) => burst(ctx, d, 440, 480, t, 1.2),
};

function master(ctx: AudioContext, volume: number): GainNode {
  const g = ctx.createGain();
  g.gain.value = volume;
  g.connect(ctx.destination);
  return g;
}

/** Start looping the ring for `phase`. Returns a stop function that silences it and releases
 * the audio context. No-op (returns a no-op stopper) where Web Audio is unavailable. */
export function playCallTone(
  phase: "incoming" | "outgoing",
  ringtone: RingtoneId,
): () => void {
  const Ctx = window.AudioContext;
  if (!Ctx) return () => {};
  // `new AudioContext()` can throw (e.g. the browser's max-contexts limit) — a ringtone is
  // never worth crashing a call over, so fail silent.
  let ctx: AudioContext;
  try {
    ctx = new Ctx();
  } catch {
    return () => {};
  }
  void ctx.resume(); // browsers may start it suspended until a gesture
  const ring = phase === "incoming" ? RINGTONES[ringtone] : RINGBACK;
  const out = master(ctx, phase === "incoming" ? 0.18 : 0.13);
  const tick = () => ring.render(ctx, out, ctx.currentTime + 0.02);
  tick();
  const id = setInterval(tick, ring.periodMs);
  return () => {
    clearInterval(id);
    void ctx.close();
  };
}

/** Play a single cycle of `ringtone` for the Settings preview, then release the context. */
export function previewRingtone(ringtone: RingtoneId): void {
  const Ctx = window.AudioContext;
  if (!Ctx) return;
  // Tapping preview repeatedly can exhaust the browser's AudioContext budget; never throw.
  let ctx: AudioContext;
  try {
    ctx = new Ctx();
  } catch {
    return;
  }
  void ctx.resume();
  const ring = RINGTONES[ringtone];
  ring.render(ctx, master(ctx, 0.18), ctx.currentTime + 0.02);
  setTimeout(() => void ctx.close(), ring.periodMs + 400);
}
