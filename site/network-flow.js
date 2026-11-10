/* Illustrative event movement, not live network or delivery telemetry. */
(() => {
  const figure = document.querySelector(".network-proof");
  if (
    !figure ||
    !Element.prototype.animate ||
    !("IntersectionObserver" in window)
  )
    return;
  const preference = matchMedia("(prefers-reduced-motion: reduce)");
  const controls = figure.querySelector(".flow-controls");
  const toggle = figure.querySelector("[data-flow-toggle]");
  const cycle = 6800;
  const tracks = [];
  let visible = false,
    userPaused = false,
    time = 0,
    animations = [];
  const put = (element, key) => {
    if (window.MeshTalkLocale) window.MeshTalkLocale.text(element, key);
    else element.textContent = key;
  };
  function addPacket(name, pathName, start, end) {
    const element = figure.querySelector(`[data-flow-packet="${name}"]`);
    const path = figure.querySelector(`#flow-${pathName}`);
    const length = path.getTotalLength();
    const at = (fraction) => {
      const point = path.getPointAtLength(length * fraction);
      return `translate(${point.x}px, ${point.y}px)`;
    };
    const frames = [{ offset: 0, opacity: 0, transform: at(0) }];
    frames.push({ offset: start, opacity: 0, transform: at(0) });
    for (let step = 1; step <= 20; step++)
      frames.push({
        offset: start + ((end - start) * step) / 20,
        opacity: step === 20 ? 0 : 1,
        transform: at(step / 20),
      });
    frames.push({ offset: 1, opacity: 0, transform: at(1) });
    tracks.push({ element, frames });
    if (!name.includes("tail")) {
      path.style.strokeDasharray = `14 ${length}`;
      tracks.push({
        element: path,
        frames: frames.map((frame) => ({
          offset: frame.offset,
          opacity: frame.opacity * 0.65,
          strokeDashoffset: `${14 - length * Math.min(1, Math.max(0, (frame.offset - start) / (end - start)))}px`,
        })),
      });
    }
  }
  addPacket("local", "local", 0.02, 0.17);
  addPacket("send", "send", 0.21, 0.36);
  addPacket("bridge", "bridge", 0.39, 0.6);
  addPacket("bridge-tail-1", "bridge", 0.405, 0.615);
  addPacket("bridge-tail-2", "bridge", 0.42, 0.63);
  addPacket("receive", "receive", 0.64, 0.79);
  for (const [name, arrival] of [
    ["local", 0.17],
    ["custody", 0.36],
    ["forward", 0.6],
    ["destination", 0.79],
  ]) {
    const element = figure.querySelector(`[data-flow-arrival="${name}"]`);
    tracks.push({
      element,
      frames: [
        { offset: 0, opacity: 0 },
        { offset: arrival, opacity: 0 },
        { offset: arrival + 0.012, opacity: 0.9 },
        { offset: arrival + 0.055, opacity: 0 },
        { offset: 1, opacity: 0 },
      ],
    });
  }
  function stop({ freeze = false, reset = false } = {}) {
    if (animations.length)
      time = Number(animations[0].currentTime ?? time) % cycle;
    for (const animation of animations) {
      if (freeze) {
        try {
          animation.commitStyles();
        } catch {
          /* Static links remain if committing SVG styles is unavailable. */
        }
      }
      animation.cancel();
    }
    animations = [];
    if (reset) time = 0;
    if (!freeze)
      tracks.forEach(({ element }) => {
        element.style.removeProperty("opacity");
        element.style.removeProperty("transform");
        element.style.removeProperty("stroke-dashoffset");
      });
  }
  function sync() {
    controls.hidden = preference.matches;
    if (preference.matches) {
      stop({ reset: true });
      figure.dataset.flowState = "reduced";
      return;
    }
    put(toggle, userPaused ? "Play data flow" : "Pause data flow");
    if (!visible || document.hidden) {
      stop();
      figure.dataset.flowState = "offscreen";
      return;
    }
    if (userPaused) {
      stop({ freeze: true });
      figure.dataset.flowState = "paused";
      return;
    }
    if (!animations.length) {
      animations = tracks.map(({ element, frames }) => {
        element.style.removeProperty("opacity");
        element.style.removeProperty("transform");
        element.style.removeProperty("stroke-dashoffset");
        const animation = element.animate(frames, {
          duration: cycle,
          iterations: Infinity,
          easing: "linear",
        });
        animation.currentTime = time;
        return animation;
      });
    }
    figure.dataset.flowState = "playing";
  }
  toggle.addEventListener("click", () => {
    userPaused = !userPaused;
    sync();
  });
  figure.querySelector("[data-flow-replay]").addEventListener("click", () => {
    stop({ reset: true });
    userPaused = false;
    sync();
  });
  const observer = new IntersectionObserver(
    (entries) => {
      visible =
        entries[0].isIntersecting && entries[0].intersectionRatio >= 0.25;
      sync();
    },
    { threshold: 0.25 },
  );
  observer.observe(figure);
  preference.addEventListener("change", sync);
  document.addEventListener("visibilitychange", sync);
  sync();
})();
