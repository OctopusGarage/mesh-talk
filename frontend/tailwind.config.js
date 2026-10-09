import animate from "tailwindcss-animate";

/** @type {import('tailwindcss').Config} */
export default {
  darkMode: ["class"],
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        border: "hsl(var(--border))",
        input: "hsl(var(--input))",
        ring: "hsl(var(--ring))",
        background: "hsl(var(--background))",
        foreground: "hsl(var(--foreground))",
        primary: {
          DEFAULT: "hsl(var(--primary))",
          foreground: "hsl(var(--primary-foreground))",
        },
        secondary: {
          DEFAULT: "hsl(var(--secondary))",
          foreground: "hsl(var(--secondary-foreground))",
        },
        destructive: {
          DEFAULT: "hsl(var(--destructive))",
          foreground: "hsl(var(--destructive-foreground))",
        },
        "destructive-fill": "hsl(var(--destructive-fill))",
        muted: {
          DEFAULT: "hsl(var(--muted))",
          foreground: "hsl(var(--muted-foreground))",
        },
        accent: {
          DEFAULT: "hsl(var(--accent))",
          foreground: "hsl(var(--accent-foreground))",
        },
        popover: {
          DEFAULT: "hsl(var(--popover))",
          foreground: "hsl(var(--popover-foreground))",
        },
        card: {
          DEFAULT: "hsl(var(--card))",
          foreground: "hsl(var(--card-foreground))",
        },
        // Signature accents — `bg-signal`, `text-verified`, `text-mention`.
        signal: "hsl(var(--signal))",
        attention: "hsl(var(--attention))",
        verified: "hsl(var(--verified))",
        mention: "hsl(var(--mention))",
        // Own chat bubble — a softer teal than the raw signal accent (kinder on the eyes
        // for a large filled surface). `bg-bubble-own`.
        "bubble-own": "hsl(var(--bubble-own))",
      },
      fontFamily: {
        display: [
          "'Space Grotesk Variable'",
          "-apple-system",
          "BlinkMacSystemFont",
          "'Segoe UI Variable'",
          "system-ui",
          "sans-serif",
        ],
        sans: [
          "-apple-system",
          "BlinkMacSystemFont",
          "'Segoe UI Variable'",
          "system-ui",
          "sans-serif",
        ],
        mono: ["'SFMono-Regular'", "Consolas", "ui-monospace", "monospace"],
      },
      boxShadow: {
        // Desktop surfaces lift only when they float above the conversation.
        elevation:
          "0 4px 16px -6px hsl(190 32% 2% / 0.34), 0 1px 3px hsl(190 32% 2% / 0.16)",
        "elevation-lg":
          "0 16px 48px -16px hsl(190 32% 2% / 0.5), 0 2px 8px hsl(190 32% 2% / 0.18)",
      },
      borderRadius: {
        lg: "var(--radius)",
        md: "calc(var(--radius) - 2px)",
        sm: "calc(var(--radius) - 4px)",
      },
      keyframes: {
        "accordion-down": {
          from: { height: "0" },
          to: { height: "var(--radix-accordion-content-height)" },
        },
        "accordion-up": {
          from: { height: "var(--radix-accordion-content-height)" },
          to: { height: "0" },
        },
      },
      animation: {
        "accordion-down": "accordion-down 0.2s ease-out",
        "accordion-up": "accordion-up 0.2s ease-out",
      },
    },
  },
  plugins: [animate],
};
