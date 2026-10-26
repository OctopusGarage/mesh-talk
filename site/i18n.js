/* One public-language owner for the landing page, 404 and versioned readers.
   English HTML is the no-JS fallback; translated resources never contain HTML. */
(() => {
  const script = document.currentScript;
  const assetBase = new URL(".", script.src);
  const supported = [
    "en",
    "es",
    "pt-BR",
    "ja",
    "ko",
    "zh-Hans",
    "zh-Hant",
    "yue",
  ];
  const preferenceKey = "mesh-talk-site-language";
  const selector = document.querySelector("#site-language");
  const status = document.querySelector("#language-status");
  if (!selector || !status) return;
  const originals = new WeakMap(),
    attributes = new WeakMap(),
    linkSources = new WeakMap();
  const owned = new Map(),
    cache = new Map();
  let statusTimer;
  let explicitLanguage = new URL(location.href).searchParams.has("lang");
  let locale = "en",
    messages = {},
    generation = 0,
    controller = null;
  const normalize = (value) => value.replace(/\s+/g, " ").trim();
  function t(source, values = {}) {
    const message = messages[source] ?? source;
    return message.replace(/\{(\w+)\}/g, (token, name) => {
      if (!Object.hasOwn(values, name)) return token;
      const value = String(values[name]);
      return messages[value] ?? value;
    });
  }
  function bind(element, name, source, values = {}) {
    if (!owned.has(element)) owned.set(element, new Map());
    const records = owned.get(element);
    if (source === null) {
      records.delete(name);
      element.removeAttribute(name);
      return;
    }
    records.set(name, { source, values });
    const result = t(source, values);
    if (name === "textContent") {
      if (element.textContent !== result) element.textContent = result;
    } else if (element.getAttribute(name) !== result)
      element.setAttribute(name, result);
  }
  const text = (element, source, values) =>
    bind(element, "textContent", source, values);
  const attr = (element, name, source, values) =>
    bind(element, name, source, values);
  function isOwnedText(element) {
    for (let current = element; current; current = current.parentElement) {
      if (owned.get(current)?.has("textContent")) return true;
    }
    return false;
  }
  function excluded(element) {
    return (
      !element ||
      element.closest(
        "script,style,pre,code,select,option,.article-heading,.article-contents a,#results li,aside nav a",
      ) ||
      (element.closest("article") && !element.closest("footer"))
    );
  }
  function translateText(node) {
    if (excluded(node.parentElement) || isOwnedText(node.parentElement)) return;
    let record = originals.get(node);
    if (!record || node.textContent !== record.last) {
      record = { source: node.textContent, last: node.textContent };
      originals.set(node, record);
    }
    const key = normalize(record.source);
    if (!key) return;
    const translated = record.source.replace(/\S(?:[\s\S]*\S)?/, () => t(key));
    record.last = translated;
    if (node.textContent !== translated) node.textContent = translated;
  }
  function translateAttributes(element) {
    if (excluded(element)) return;
    if (!attributes.has(element)) attributes.set(element, new Map());
    const records = attributes.get(element);
    for (const name of ["aria-label", "title", "placeholder", "alt"]) {
      if (!element.hasAttribute(name) || owned.get(element)?.has(name))
        continue;
      const value = element.getAttribute(name);
      let record = records.get(name);
      if (!record || value !== record.last) {
        record = { source: value, last: value };
        records.set(name, record);
      }
      record.last = t(record.source);
      if (value !== record.last) element.setAttribute(name, record.last);
    }
  }
  function translateLinks(element) {
    if (!element.matches("a[href]")) return;
    const actual = element.getAttribute("href");
    let record = linkSources.get(element);
    if (!record || actual !== record.last) {
      record = { source: actual, last: actual };
      linkSources.set(element, record);
    }
    if (record.source.startsWith("#")) return;
    const url = new URL(record.source, location.href);
    if (
      url.origin !== location.origin ||
      !/\/(?:[^/.]+\.html)?$/.test(url.pathname)
    )
      return;
    const automaticEnglish = locale === "en" && !explicitLanguage;
    if (!automaticEnglish) url.searchParams.set("lang", locale);
    // Explicit English needs the same storage-independent navigation as other locales.
    record.last = automaticEnglish
      ? record.source
      : url.pathname + url.search + url.hash;
    if (actual !== record.last) element.setAttribute("href", record.last);
  }
  function visit(root) {
    if (root.nodeType === Node.TEXT_NODE) {
      translateText(root);
      return;
    }
    if (!(root instanceof Element)) return;
    const elements = [root, ...root.querySelectorAll("*")];
    for (const element of elements) {
      translateAttributes(element);
      translateLinks(element);
      for (const child of element.childNodes)
        if (child.nodeType === Node.TEXT_NODE) translateText(child);
    }
  }
  function apply() {
    document.documentElement.lang = locale;
    visit(document.body);
    for (const [element, records] of owned) {
      if (!element.isConnected) {
        owned.delete(element);
        continue;
      }
      for (const [name, record] of records)
        bind(element, name, record.source, record.values);
    }
    document.querySelectorAll("[data-version]").forEach((element) =>
      text(element, "Version {version} · English", {
        version: element.dataset.version,
      }),
    );
    document.querySelectorAll(".help-language-note").forEach((element) => {
      element.hidden = locale === "en";
    });
    document.querySelectorAll("article footer").forEach((element) => {
      element.lang = locale;
    });
    selector.value = locale;
  }
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      if (record.type === "childList") record.addedNodes.forEach(visit);
      else if (record.type === "characterData") translateText(record.target);
      else {
        translateAttributes(record.target);
        translateLinks(record.target);
      }
    }
  });
  function placeholders(value) {
    return [...value.matchAll(/\{\w+\}/g)]
      .map((x) => x[0])
      .sort()
      .join(",");
  }
  async function dictionary(code, signal) {
    if (cache.has(code)) return cache.get(code);
    const response = await fetch(new URL(`locales/${code}.json`, assetBase), {
      signal,
    });
    if (!response.ok) throw Error("Unavailable language");
    const result = await response.json();
    if (!result || typeof result !== "object" || Array.isArray(result))
      throw Error("Invalid language");
    return result;
  }
  function resolveLanguage(tag) {
    if (typeof tag !== "string") return null;
    const lower = tag.trim().toLowerCase();
    const exact = supported.find((code) => code.toLowerCase() === lower);
    if (exact) return exact;
    if (/^pt(?:-|$)/.test(lower)) return "pt-BR";
    if (/^ko(?:-|$)/.test(lower)) return "ko";
    if (/^zh-(?:tw|hk|mo|hant)(?:-|$)/.test(lower)) return "zh-Hant";
    if (/^zh(?:-|$)/.test(lower)) return "zh-Hans";
    return supported.find((code) => code === lower.split("-")[0]) ?? null;
  }
  async function setLocale(code, { initial = false } = {}) {
    code = resolveLanguage(code) ?? "en";
    const current = ++generation;
    clearTimeout(statusTimer);
    controller?.abort();
    controller = new AbortController();
    const request = controller;
    selector.value = code;
    if (!initial) text(status, "Loading language…");
    const timeout = setTimeout(() => request.abort(), 8000);
    try {
      let next = {};
      if (code !== "en") {
        const [english, translated] = await Promise.all([
          dictionary("en", request.signal),
          dictionary(code, request.signal),
        ]);
        const keys = Object.keys(english);
        if (
          Object.keys(translated).length !== keys.length ||
          !keys.every(
            (key) =>
              typeof translated[key] === "string" &&
              translated[key].trim() &&
              placeholders(key) === placeholders(translated[key]),
          )
        )
          throw Error("Incomplete language");
        cache.set("en", english);
        cache.set(code, translated);
        next = translated;
      }
      if (current !== generation) return;
      messages = next;
      locale = code;
      explicitLanguage ||= !initial;
      apply();
      const url = new URL(location.href);
      // Preserve an explicit English choice and every existing search/hash.
      if (!initial || url.searchParams.has("lang")) {
        url.searchParams.set("lang", code);
        history.replaceState(null, "", url);
      }
      try {
        // Browser detection remains automatic until an explicit selection or URL.
        if (!initial || url.searchParams.has("lang"))
          localStorage.setItem(preferenceKey, code);
      } catch {
        /* URL and navigation still work. */
      }
      if (initial) text(status, "");
      else
        text(status, "Language changed to {language}.", {
          language: selector.selectedOptions[0].textContent,
        });
      if (!initial)
        statusTimer = setTimeout(() => {
          if (current === generation) text(status, "");
        }, 3500);
      document.dispatchEvent(
        new CustomEvent("mesh-language-change", { detail: code }),
      );
    } catch {
      if (current !== generation) return;
      selector.value = locale;
      text(status, "This language couldn’t load. Try choosing it again.");
    } finally {
      clearTimeout(timeout);
    }
  }
  window.MeshTalkLocale = {
    t,
    text,
    attr,
    apply,
    setLocale,
    get locale() {
      return locale;
    },
    binding(element, name) {
      return (
        owned.get(element)?.get(name) ?? {
          source:
            attributes.get(element)?.get(name)?.source ??
            element.getAttribute(name),
          values: {},
        }
      );
    },
    source(element, name) {
      return (
        owned.get(element)?.get(name)?.source ??
        attributes.get(element)?.get(name)?.source ??
        element.getAttribute(name)
      );
    },
  };
  document.querySelectorAll(".language-control").forEach((element) => {
    element.hidden = false;
  });
  apply();
  observer.observe(document.body, {
    subtree: true,
    childList: true,
    characterData: true,
    attributes: true,
    attributeFilter: ["aria-label", "title", "placeholder", "alt", "href"],
  });
  selector.addEventListener("change", () => setLocale(selector.value));
  function preferred() {
    const url = new URL(location.href);
    if (url.searchParams.has("lang")) return url.searchParams.get("lang");
    try {
      const saved = localStorage.getItem(preferenceKey);
      const resolved = resolveLanguage(saved);
      if (resolved) return resolved;
    } catch {
      /* Use browser preference. */
    }
    for (const language of navigator.languages ?? [navigator.language]) {
      const resolved = resolveLanguage(language);
      if (resolved) return resolved;
    }
    return "en";
  }
  setLocale(preferred(), { initial: true });
})();
