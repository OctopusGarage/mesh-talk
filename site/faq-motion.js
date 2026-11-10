/* Native details remain the source of truth; motion only bridges their height. */
(() => {
  if (typeof Element.prototype.animate !== "function") return;
  const preference = matchMedia("(prefers-reduced-motion: reduce)");
  const controllers = [];

  document.querySelectorAll(".faq-list details").forEach((details) => {
    const summary = details.querySelector(":scope > summary");
    if (!summary) return;
    const content = document.createElement("div");
    content.className = "faq-motion-content";
    while (summary.nextSibling) content.append(summary.nextSibling);
    details.append(content);
    let intendedOpen = details.open;
    let animation = null;
    const readLayout = () => [
      details.getBoundingClientRect().width,
      summary.getBoundingClientRect().height,
      content.getBoundingClientRect().height,
    ];
    let layout = readLayout();

    const observer = new MutationObserver(() => settle(details.open));
    observer.observe(details, { attributes: true, attributeFilter: ["open"] });

    function setOpen(open) {
      details.open = open;
      observer.takeRecords();
    }

    function settle(open = intendedOpen) {
      intendedOpen = open;
      const previous = animation;
      animation = null;
      previous?.cancel();
      setOpen(open);
      delete details.dataset.faqAnimating;
      content.inert = false;
      summary.removeAttribute("aria-expanded");
      layout = readLayout();
    }

    function toggle(event) {
      if (event.defaultPrevented) return;
      // Native summary activation supplies click for pointer, Enter and Space.
      if (preference.matches || document.hidden) return;
      event.preventDefault();
      const from = details.getBoundingClientRect().height;
      intendedOpen = !intendedOpen;
      animation?.cancel();
      animation = null;
      setOpen(true);
      delete details.dataset.faqAnimating;
      const naturalHeight = details.getBoundingClientRect().height;
      const style = getComputedStyle(details);
      const closedHeight =
        summary.getBoundingClientRect().height +
        parseFloat(style.borderTopWidth) +
        parseFloat(style.borderBottomWidth) +
        parseFloat(style.paddingTop) +
        parseFloat(style.paddingBottom);
      const to = intendedOpen ? naturalHeight : closedHeight;
      if (!intendedOpen && content.contains(document.activeElement))
        summary.focus({ preventScroll: true });
      content.inert = !intendedOpen;
      summary.setAttribute("aria-expanded", String(intendedOpen));
      details.dataset.faqAnimating = "";
      layout = readLayout();
      const current = details.animate(
        [{ height: `${from}px` }, { height: `${to}px` }],
        {
          duration: intendedOpen ? 270 : 190,
          easing: "cubic-bezier(0.2, 0.7, 0.2, 1)",
        },
      );
      animation = current;
      current.finished.then(
        () => {
          if (animation === current) settle();
        },
        () => {},
      );
    }

    summary.addEventListener("click", toggle);

    // Locale replacement and wrapping changes must never leave a measured
    // pixel height clipping the newly sized answer.
    const textObserver = new MutationObserver(() => {
      if (animation) settle();
    });
    textObserver.observe(content, {
      subtree: true,
      characterData: true,
      childList: true,
    });
    if ("ResizeObserver" in window) {
      const resize = new ResizeObserver(() => {
        const nextLayout = readLayout();
        if (
          animation &&
          nextLayout.some((value, index) => value !== layout[index])
        )
          settle();
        layout = readLayout();
      });
      resize.observe(details);
      resize.observe(summary);
      resize.observe(content);
    }
    controllers.push({ details, settle });
  });

  const settleAll = () => controllers.forEach(({ settle }) => settle());
  preference.addEventListener("change", settleAll);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) settleAll();
  });
  new MutationObserver(settleAll).observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["lang"],
  });
  addEventListener("hashchange", () => {
    const target = controllers.find(
      ({ details }) => `#${details.id}` === location.hash,
    );
    target?.settle(true);
  });
})();
