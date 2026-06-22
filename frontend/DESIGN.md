# Ink & Signal — Mesh-Talk design system

The foundation visual contract for the React/TS/Tailwind/shadcn frontend. Screen-level
agents restyle against what's documented here. Keep it cohesive: spend boldness on the
SIGNATURE (cryptographic identity + LAN presence); keep everything else quiet.

Direction: a calm, premium, trust-forward secure messenger. Sustained ink surfaces, ONE
confident teal "signal" accent, cryptographic identity rendered beautifully, living LAN
presence. **Dark is the hero**; light is a calm cool-paper; OLED is true-black dark.

---

## Palette

Tokens live in `src/index.css` as shadcn HSL CSS-vars (`hsl(var(--x))`). Three themes are
toggled by `lib/theme.ts` via the `.dark` / `.oled` classes on `<html>`. Use the Tailwind
color utilities (`bg-card`, `text-muted-foreground`, …) — never hard-code hex.

| Var                                    | Role                | Dark (hero)                  | Light                    |
| -------------------------------------- | ------------------- | ---------------------------- | ------------------------ |
| `--background`                         | app canvas          | deep ink `220 24% 7%`        | cool paper `210 20% 98%` |
| `--foreground`                         | primary text        | `220 14% 92%`                | `220 28% 10%`            |
| `--card` / `--popover`                 | raised surfaces     | `220 22% 10%` / `220 24% 9%` | white                    |
| `--primary` / `--signal`               | **the teal accent** | `172 66% 50%`                | `174 72% 33%`            |
| `--secondary` / `--muted` / `--accent` | quiet fills         | `220 18% 14–17%`             | `215 20–24% 92–95%`      |
| `--muted-foreground`                   | secondary text      | `218 12% 62%`                | `220 10% 42%`            |
| `--border` / `--input` / `--ring`      | structure / focus   | `220 18–20% 16–18%`          | `215 20% 86–88%`         |
| `--destructive`                        | danger              | `0 72% 56%`                  | `0 72% 48%`              |

Custom signature accents (tuned per theme for contrast):

| Var          | Tailwind                          | Use                                                         |
| ------------ | --------------------------------- | ----------------------------------------------------------- |
| `--signal`   | `bg-signal` `text-signal`         | the teal accent; online presence; primary actions           |
| `--verified` | `text-verified` `border-verified` | crypto **verified** state — safety numbers, verified badges |
| `--mention`  | `text-mention` `bg-mention`       | @mentions, away/recent presence (amber)                     |

OLED overrides only the surface/border/input hues to true-black + cooler ink greys.

Other tokens: `--radius: 0.625rem` (Tailwind `rounded-lg`/`md`/`sm` derive from it).
`::selection` is teal-tinted. Scrollbars are subtle. `prefers-reduced-motion` neutralizes
all CSS animation/transition globally (see Motion).

---

## Typography

Bundled **locally** via `@fontsource-variable/*` (offline desktop app, CSP-safe — no CDN);
imported in `main.tsx`. Wired in `tailwind.config.js` `fontFamily`.

| Role    | Tailwind                          | Family        | When                                                                                                  |
| ------- | --------------------------------- | ------------- | ----------------------------------------------------------------------------------------------------- |
| Display | `font-display`                    | Space Grotesk | headings, dialog titles, names in crests                                                              |
| Body    | `font-sans` (default on `<body>`) | Inter         | all running text, labels, UI                                                                          |
| Mono    | `font-mono`                       | Geist Mono    | **everything cryptographic/identity** — fingerprints, safety numbers, IDs, ports, `host:port`, hashes |

Rule: **anything that is an identifier or crypto material is mono.** If a user might
compare it character-by-character, it's mono.

---

## Motion

`framer-motion`. Shared variants/transitions in `src/lib/motion.ts`. Restrained: durations
120–260ms, tasteful eases. **Always gate JS-driven motion on `useMotionOK()`** — the global
CSS covers CSS transitions/animations under reduced-motion, but framer animation must be
disabled in JS too.

```tsx
import { motion } from "framer-motion";
import { fadeSlideUp, listStagger, useMotionOK } from "@/lib/motion";

const ok = useMotionOK();
<motion.div
  initial={ok ? "hidden" : false}
  animate="visible"
  variants={fadeSlideUp}
/>;
```

Exports: `fadeSlideUp` (workhorse content entrance), `fade` (overlays), `listStagger`
(list container; pair children with `fadeSlideUp`), `sheet` (edge panels), `popIn`
(dialog/popover), plus `ease`, `spring`, `transition`, `transitionFast`, and the
`useMotionOK()` hook.

---

## Signature components — `src/components/identity/`

Import from `@/components/identity`. Dependency-light, themed via the tokens.

### `IdentityGlyph`

Deterministic on-brand SVG sigil — a horizontally-symmetric "mesh-node" lattice, hue
derived from the seed but harmonized into the teal/ink arc (never garish). Same seed →
same glyph. This is the core avatar everywhere.

```tsx
<IdentityGlyph seed={fingerprint} size={40} verified />
```

Props: `seed: string` (id/fingerprint), `size?: number` (default 40), `verified?: boolean`,
`className?`, `title?`.

### `PresenceDot`

Status dot with a soft "breathing" pulse when online (halts under reduced motion). Online
= signal teal, away/recent = mention amber, offline = dim muted.

```tsx
<PresenceDot status="online" size="md" />
```

Props: `status?: "online" | "away" | "recent" | "offline"` (default offline),
`size?: "sm" | "md" | "lg"`, `label?`, `className?`.

### `IdentityCrest`

Composes glyph + display-font name + short mono id (first 4 … last 4) + optional verified
badge and presence overlay. `compact` for rows/headers, `large` for profile/verify panels.

```tsx
<IdentityCrest
  id={fingerprint}
  name="Alice"
  verified
  status="online"
  variant="compact"
/>
```

Props: `id: string`, `name: string`, `verified?: boolean`,
`status?: PresenceStatus`, `variant?: "compact" | "large"`, `className?`.

### `SafetyNumber`

Reusable presentation of a fingerprint / safety number in grouped mono blocks with a
verified visual state and optional word sequence. Pure presentation (computation stays in
the backend / `VerifyContactDialog`).

```tsx
<SafetyNumber value={sn.grouped} words={sn.words} verified={trust.verified} />
```

Props: `value: string`, `words?: string[]`, `verified?: boolean`, `className?`.

---

## Base primitives — `src/components/ui/`

shadcn primitives refined under the new tokens. **APIs/props/exports unchanged** — only
visual refinement (teal focus-visible rings, smooth `ease-out` hover/active transitions,
subtle `active:scale` press, consistent radius, `shadow-elevation` on cards/dialogs/
popovers). Notables:

- **Button** — `shadow-sm` on solid variants, press scale, teal ring.
- **Input** — hover border hint + teal focus border/ring.
- **Dialog** — `shadow-elevation-lg`, title uses `font-display`, tuned zoom/fade.
- **Popover** — `shadow-elevation`.
- **Avatar** — renders `src` image when given; **falls back to `IdentityGlyph`** otherwise.
  Existing `name`/`id`/`className` API intact; `src`/`size`/`verified` are additive. Size
  via `className` (`h-9 w-9`) still works (CSS overrides the SVG dimensions).
- **Tabs / Switch / Badge / Label** — token-aligned, unchanged APIs.

Elevation tokens: `shadow-elevation` (cards/popovers), `shadow-elevation-lg` (dialogs/sheets).
