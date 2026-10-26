/* Optional motion layer. Content and native interactions never wait for it. */
(() => {
  if (typeof Element.prototype.animate !== "function") return;

  const preference = matchMedia("(prefers-reduced-motion: reduce)");
  const active = new Map();
  const generations = new WeakMap();
  const easing = "cubic-bezier(0.16, 1, 0.3, 1)";
  let epoch = 0;

  function cancel(element) {
    active.get(element)?.cancel();
    active.delete(element);
  }

  function play(element, frames, duration, delay = 0) {
    if (!element || preference.matches || document.hidden) return;
    cancel(element);
    const animation = element.animate(frames, {
      duration,
      delay,
      easing,
      fill: "backwards",
    });
    active.set(element, animation);
    // A cancelled animation rejects finished; always consume both outcomes.
    animation.finished.then(
      () => {
        if (active.get(element) === animation) active.delete(element);
      },
      () => {},
    );
  }

  function cancelAll() {
    epoch++;
    for (const element of active.keys()) cancel(element);
  }

  preference.addEventListener("change", cancelAll);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) cancelAll();
  });

  // The ownership statement resolves first; the real app settles into its
  // frame. Run once at the top of a visit, never on section navigation.
  if (!location.hash && window.scrollY < 100) {
    play(
      document.querySelector(".hero h1 span"),
      [{ opacity: 0.45 }, { opacity: 1 }],
      520,
    );
    play(
      document.querySelector(".preview-link"),
      [
        { transform: "scale(0.985)", opacity: 0.72 },
        { transform: "scale(1)", opacity: 1 },
      ],
      640,
      60,
    );
  }

  // The source of truth changes immediately in site.js. Decode only to avoid
  // animating an empty bitmap; rapid changes invalidate older decode work.
  document.addEventListener("mesh-preview-change", async (event) => {
    const image = event.detail?.element;
    if (!(image instanceof HTMLImageElement)) return;
    const generation = (generations.get(image) ?? 0) + 1;
    generations.set(image, generation);
    cancel(image);
    if (image.id === "preview-image") cancel(image.closest(".preview-link"));
    if (preference.matches || document.hidden) return;
    const currentEpoch = epoch;
    const source = image.src;
    try {
      await image.decode();
    } catch {
      return;
    }
    if (
      generations.get(image) !== generation ||
      currentEpoch !== epoch ||
      image.src !== source ||
      !image.isConnected ||
      image.closest("dialog:not([open])")
    )
      return;
    const distance = event.detail.direction === -1 ? -4 : 4;
    play(
      image,
      [
        { opacity: 0.85, transform: `translateX(${distance}px)` },
        { opacity: 1, transform: "translateX(0)" },
      ],
      180,
    );
  });

  document.querySelectorAll("dialog").forEach((dialog) => {
    const observer = new MutationObserver(() => {
      if (!dialog.open) {
        cancelDialog();
        return;
      }
      play(
        dialog,
        [
          { opacity: 0.65, transform: "translateY(8px) scale(0.99)" },
          { opacity: 1, transform: "translateY(0) scale(1)" },
        ],
        240,
      );
    });
    function cancelDialog() {
      cancel(dialog);
      dialog.querySelectorAll("img").forEach((image) => {
        generations.set(image, (generations.get(image) ?? 0) + 1);
        cancel(image);
      });
    }
    observer.observe(dialog, { attributes: true, attributeFilter: ["open"] });
    dialog.addEventListener("close", cancelDialog);
  });
})();
