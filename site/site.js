/* Progressive enhancement: links, screenshots, FAQs and platform content work
   without JavaScript. No analytics or runtime dependencies. Release downloads are owned by downloads.js. */
(() => {
  const put = (element, source, values = {}) => {
    if (window.MeshTalkLocale)
      window.MeshTalkLocale.text(element, source, values);
    else
      element.textContent = source.replace(
        /\{(\w+)\}/g,
        (_, key) => values[key] ?? `{${key}}`,
      );
  };
  const putAttribute = (element, name, source, values = {}) => {
    if (window.MeshTalkLocale)
      window.MeshTalkLocale.attr(element, name, source, values);
    else if (source === null) element.removeAttribute(name);
    else
      element.setAttribute(
        name,
        source.replace(/\{(\w+)\}/g, (_, key) => values[key] ?? `{${key}}`),
      );
  };

  const header = document.querySelector(".nav");
  const menu = header.querySelector(".menu-toggle");
  menu.hidden = false;
  header.dataset.enhanced = "";
  const closeMenu = () => menu.setAttribute("aria-expanded", "false");
  menu.addEventListener("click", () => {
    menu.setAttribute(
      "aria-expanded",
      String(menu.getAttribute("aria-expanded") !== "true"),
    );
  });
  header.querySelectorAll("nav a").forEach((link) =>
    link.addEventListener("click", () => {
      closeMenu();
      if (!mobile.matches || !link.hash || link.origin !== location.origin)
        return;
      const destination = document.getElementById(link.hash.slice(1));
      if (destination) {
        destination.tabIndex = -1;
        destination.focus({ preventScroll: true });
      }
    }),
  );
  header.addEventListener("keydown", (event) => {
    if (
      event.key === "Escape" &&
      menu.getAttribute("aria-expanded") === "true"
    ) {
      closeMenu();
      if (mobile.matches) menu.focus();
    }
  });
  document.addEventListener("click", (event) => {
    if (!header.contains(event.target)) closeMenu();
  });
  const mobile = matchMedia("(max-width: 1100px)");
  mobile.addEventListener("change", () => {
    closeMenu();
    if (
      mobile.matches &&
      header.querySelector("nav").contains(document.activeElement)
    )
      menu.focus();
  });

  const bitmaps = new Map();
  const denseSource = (source) => source.replace(/\.webp$/, "@2x.webp");
  const candidates = (source) =>
    `${source} 1280w, ${denseSource(source)} 2560w`;
  function loadBitmap(source, responsive = null) {
    const url = new URL(source, location.href).href;
    const key = responsive
      ? `${url}|${responsive.sizes}|${innerWidth}|${devicePixelRatio}`
      : url;
    if (!bitmaps.has(key)) {
      const image = new Image();
      if (responsive) {
        image.sizes = responsive.sizes;
        image.srcset = responsive.srcset;
      }
      image.src = url;
      const decoded = image.decode().catch((error) => {
        bitmaps.delete(key);
        throw error;
      });
      bitmaps.set(key, decoded);
    }
    return bitmaps.get(key);
  }

  // Arrow, Home and End keys move focus and selection together in each tablist.
  function tabs(list, activate) {
    const items = [...list.querySelectorAll('[role="tab"]')];
    const select = (item) => {
      const commit = () =>
        items.forEach((tab) => {
          tab.setAttribute("aria-selected", String(tab === item));
          tab.tabIndex = tab === item ? 0 : -1;
        });
      const activation = activate(item);
      if (activation?.then)
        activation
          .then((accepted) => {
            if (accepted !== false) commit();
          })
          .catch(() => {});
      else commit();
    };
    list.hidden = false;
    items.forEach((item) => item.addEventListener("click", () => select(item)));
    list.addEventListener("keydown", (event) => {
      const current = items.indexOf(document.activeElement);
      if (current < 0) return;
      let next;
      if (event.key === "ArrowRight") next = (current + 1) % items.length;
      if (event.key === "ArrowLeft")
        next = (current - 1 + items.length) % items.length;
      if (event.key === "Home") next = 0;
      if (event.key === "End") next = items.length - 1;
      if (next === undefined) return;
      event.preventDefault();
      select(items[next]);
      items[next].focus();
    });
    select(items[0]);
    return select;
  }

  const preview = document.querySelector(".customization-link");
  const previewImage = document.querySelector("#customization-image");
  const imagePanel = document.querySelector("#customization-panel");
  const themeNames = {
    messi: "Messi",
    barcelona: "Barcelona",
    argentina: "Argentina",
  };
  let themeRequest = 0;
  const previewStatus = document.querySelector(".preview-status");
  document.querySelectorAll("[data-theme]").forEach((item) => {
    const preload = () => {
      const src = `screenshots/hero-${item.dataset.theme}.webp`;
      return loadBitmap(src, {
        srcset: candidates(src),
        sizes: previewImage.sizes,
      }).catch(() => {});
    };
    item.addEventListener("pointerenter", preload);
    item.addEventListener("focus", preload);
  });
  tabs(document.querySelector(".preview-tabs"), async (item) => {
    const request = ++themeRequest;
    const name = item.dataset.theme;
    const src = `screenshots/hero-${name}.webp`;
    imagePanel.setAttribute("aria-busy", "true");
    try {
      const responsive = { srcset: candidates(src), sizes: previewImage.sizes };
      await loadBitmap(src, responsive);
      if (request !== themeRequest) return false;
      previewImage.srcset = responsive.srcset;
      previewImage.src = src;
      putAttribute(
        previewImage,
        "alt",
        "An optional {theme} theme installed in Mesh-Talk. Demo data.",
        { theme: themeNames[name] },
      );
      preview.href = denseSource(src);
      imagePanel.setAttribute("aria-labelledby", item.id);
      put(previewStatus, "");
      document.dispatchEvent(
        new CustomEvent("mesh-preview-change", {
          detail: { element: previewImage, direction: 1 },
        }),
      );
      return true;
    } catch {
      if (request === themeRequest)
        put(previewStatus, "Preview couldn’t load. Choose it again to retry.");
      return false;
    } finally {
      if (request === themeRequest) imagePanel.removeAttribute("aria-busy");
    }
  });

  const selectPlatform = tabs(
    document.querySelector(".platform-tabs"),
    (item) => {
      document.dispatchEvent(
        new CustomEvent("mesh-platform-selected", {
          detail: item.dataset.platform,
        }),
      );
      document.querySelectorAll(".platform").forEach((panel) => {
        panel.setAttribute("role", "tabpanel");
        panel.tabIndex = 0;
        panel.hidden = panel.id !== item.getAttribute("aria-controls");
      });
    },
  );

  document.addEventListener("mesh-platform-detected", (event) => {
    const item = document.querySelector(`[data-platform="${event.detail}"]`);
    if (item) selectPlatform(item);
  });

  const sectionLinks = [
    ...document.querySelectorAll('.nav__links a[href^="#"]'),
  ];
  if ("IntersectionObserver" in window) {
    const sections = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (!entry.isIntersecting) return;
          sectionLinks.forEach((link) => {
            if (link.hash === `#${entry.target.id}`)
              link.setAttribute("aria-current", "location");
            else link.removeAttribute("aria-current");
          });
        });
      },
      { rootMargin: "-15% 0px -55% 0px" },
    );
    sectionLinks.forEach((link) => {
      const section = document.getElementById(link.hash.slice(1));
      if (section) sections.observe(section);
    });
  }

  // Native fragment navigation can reach a closed details element. Open the
  // owning answer on entry, hash changes and browser history navigation.
  function openAnswer() {
    const answer = document.getElementById(location.hash.slice(1));
    if (!answer?.matches(".faq-list details")) return;
    answer.open = true;
    answer.scrollIntoView({ block: "start" });
  }
  openAnswer();
  addEventListener("hashchange", openAnswer);

  // One copy interaction for source commands and installation commands. Closed
  // dialogs invalidate pending clipboard work so it cannot update a new visit.
  document.querySelectorAll("[data-copy-target]").forEach((button) => {
    const code = document.getElementById(button.dataset.copyTarget);
    const feedback = document.getElementById(button.dataset.copyStatus);
    const owner = button.closest("dialog");
    let generation = 0;
    owner?.addEventListener("close", () => {
      generation++;
      button.disabled = false;
    });
    button.addEventListener("click", async () => {
      const current = ++generation;
      button.disabled = true;
      put(feedback, "");
      try {
        await navigator.clipboard.writeText(code.textContent);
        if (current === generation)
          put(feedback, owner ? "Command copied." : "Commands copied.");
      } catch {
        if (current !== generation) return;
        const range = document.createRange();
        range.selectNodeContents(code);
        const selection = window.getSelection();
        selection?.removeAllRanges();
        selection?.addRange(range);
        put(
          feedback,
          "Copy isn’t available. The command is selected; press Control+C or Command+C to copy.",
        );
      } finally {
        if (current === generation) button.disabled = false;
      }
    });
  });

  const viewer = document.querySelector(".image-viewer");
  if (typeof viewer.showModal === "function") {
    const image = viewer.querySelector("img");
    image.draggable = false;
    let slides = [],
      index = 0,
      opener = null,
      gesture = null,
      requestedIndex = 0,
      slideRequest = 0;
    const show = async (next) => {
      const request = ++slideRequest;
      requestedIndex = (next + slides.length) % slides.length;
      const targetIndex = requestedIndex;
      const direction = next < index ? -1 : 1;
      const slide = slides[targetIndex];
      viewer.setAttribute("aria-busy", "true");
      const loading = viewer.querySelector(".viewer-loading");
      put(loading, "Loading preview…");
      loading.hidden = false;
      try {
        await loadBitmap(slide.src);
        if (request !== slideRequest || !viewer.open) return;
        index = targetIndex;
        image.src = slide.src;
        putAttribute(image, "alt", slide.alt, slide.altValues);
        put(
          viewer.querySelector(".viewer-caption"),
          slide.alt,
          slide.altValues,
        );
        viewer.querySelector(".viewer-position").textContent =
          `${index + 1} / ${slides.length}`;
        viewer.querySelector("[data-viewer-original]").href = slide.src;
        loading.hidden = true;
        document.dispatchEvent(
          new CustomEvent("mesh-preview-change", {
            detail: { element: image, direction },
          }),
        );
      } catch {
        if (request === slideRequest)
          put(loading, "Preview couldn’t load. Choose it again to retry.");
      } finally {
        if (request === slideRequest) viewer.removeAttribute("aria-busy");
      }
    };
    viewer.querySelector(".viewer-controls").hidden = false;
    document.querySelectorAll("a[data-zoom]").forEach((link) => {
      link.addEventListener("click", (event) => {
        if (
          event.button !== 0 ||
          event.metaKey ||
          event.ctrlKey ||
          event.shiftKey ||
          event.altKey
        )
          return;
        event.preventDefault();
        opener = link;
        // Resolve the current preview theme at open time and de-duplicate the
        // repeated gallery captures while keeping their original captions.
        const unique = new Map();
        document.querySelectorAll("a[data-zoom]").forEach((item) => {
          if (!unique.has(item.href)) {
            const binding = window.MeshTalkLocale?.binding(
              item.querySelector("img"),
              "alt",
            ) ?? { source: item.querySelector("img").alt, values: {} };
            unique.set(item.href, {
              src: item.href,
              alt: binding.source,
              altValues: binding.values,
            });
          }
        });
        slides = [...unique.values()];
        show(slides.findIndex((slide) => slide.src === link.href));
        viewer.showModal();
      });
    });
    viewer
      .querySelector("[data-viewer-prev]")
      .addEventListener("click", () => show(requestedIndex - 1));
    viewer
      .querySelector("[data-viewer-next]")
      .addEventListener("click", () => show(requestedIndex + 1));
    viewer.addEventListener("keydown", (event) => {
      if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
        event.preventDefault();
        show(requestedIndex + (event.key === "ArrowRight" ? 1 : -1));
      }
    });
    image.addEventListener("pointerdown", (event) => {
      if (event.pointerType !== "touch" || !event.isPrimary) return;
      gesture = { id: event.pointerId, x: event.clientX, y: event.clientY };
      image.setPointerCapture(event.pointerId);
    });
    image.addEventListener("pointerup", (event) => {
      if (gesture?.id !== event.pointerId) return;
      const dx = event.clientX - gesture.x,
        dy = event.clientY - gesture.y;
      gesture = null;
      if (Math.abs(dx) > 48 && Math.abs(dx) > Math.abs(dy) * 1.5)
        show(requestedIndex + (dx < 0 ? 1 : -1));
    });
    ["pointercancel", "lostpointercapture"].forEach((type) =>
      image.addEventListener(type, () => {
        gesture = null;
      }),
    );
    viewer.addEventListener("close", () => {
      gesture = null;
      slideRequest++;
      viewer.removeAttribute("aria-busy");
      opener?.focus({ preventScroll: true });
    });
    viewer.addEventListener("click", (event) => {
      if (event.target !== viewer) return;
      const bounds = viewer.getBoundingClientRect();
      if (
        event.clientX < bounds.left ||
        event.clientX > bounds.right ||
        event.clientY < bounds.top ||
        event.clientY > bounds.bottom
      )
        viewer.close();
    });
  }
})();
