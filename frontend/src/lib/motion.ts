/**
 * "Ink & Signal" shared motion — restrained, silky, reduced-motion aware.
 *
 * Durations stay short (120–260ms) and eases are tasteful. The global CSS already
 * neutralizes CSS transitions/animations under `prefers-reduced-motion`; this module
 * covers JS-driven (framer-motion) animation. Use `useMotionOK()` to no-op motion when
 * the user prefers reduced motion.
 */
import {
  useReducedMotion,
  type Transition,
  type Variants,
} from "framer-motion";

/** Default tasteful ease (a soft, slightly-overshootless out-curve). */
export const ease = [0.22, 1, 0.36, 1] as const;

/** Standard timed transition for entrances. */
export const transition: Transition = { duration: 0.22, ease };

/** A short, snappy transition for hover/tap micro-interactions. */
const transitionFast: Transition = { duration: 0.13, ease };

/** Fade + small upward slide — the workhorse entrance for content. */
export const fadeSlideUp: Variants = {
  hidden: { opacity: 0, y: 8 },
  visible: { opacity: 1, y: 0, transition },
  exit: { opacity: 0, y: 8, transition: transitionFast },
};

/** Stagger container for lists — pair with `fadeSlideUp` on each child. */
export const listStagger: Variants = {
  hidden: {},
  visible: { transition: { staggerChildren: 0.04, delayChildren: 0.02 } },
};

/**
 * Returns `true` when JS-driven motion should run, `false` when the user prefers
 * reduced motion. Components should gate animation props on this:
 *
 *   const ok = useMotionOK();
 *   <motion.div initial={ok ? "hidden" : false} animate="visible" variants={fadeSlideUp} />
 */
export function useMotionOK(): boolean {
  return !useReducedMotion();
}
