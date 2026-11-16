import tailwindcss from "@tailwindcss/postcss";

export default {
  plugins: [
    // Vite owns final CSS optimization. Lower alpha before either optimizer
    // can flatten the generated color-mix feature guards.
    tailwindcss({ optimize: false }),
    {
      postcssPlugin: "mesh-talk-theme-alpha",
      OnceExit(root) {
        // Older WKWebView falls back to an opaque theme color for color-mix(),
        // hiding same-color SVG strokes. Our HSL channel tokens support alpha
        // directly; lower only these transparent mixes before Vite minifies CSS.
        root.walkDecls((decl) => {
          const compatible = decl.value.replace(
            /color-mix\(in (?:oklab|srgb),\s*hsl\(var\((--[\w-]+)\)\)\s+([\d.]+)%,\s*transparent\)/g,
            "hsl(var($1) / $2%)",
          );
          if (compatible === decl.value) return;
          decl.value = compatible;
          // Tailwind emits an opaque fallback followed by this nested guard.
          // Direct HSL alpha needs no color-mix feature; let it override the
          // fallback unconditionally, preserving surrounding hover/dark rules.
          const guard = decl.parent;
          if (
            guard.type === "atrule" &&
            guard.name === "supports" &&
            /^\(color:\s*color-mix\(in lab,\s*red,\s*red\)\)$/.test(
              guard.params,
            )
          ) {
            guard.before(decl);
            if (guard.nodes.length === 0) guard.remove();
          }
        });
      },
    },
  ],
};
