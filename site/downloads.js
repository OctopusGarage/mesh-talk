/* Public Stable download adapter. The release workflow owns package names and
   publication; the API supplies the exact immutable asset URLs and sizes. */
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

  const releases = "https://github.com/OctopusGarage/mesh-talk/releases";
  const endpoint =
    "https://api.github.com/repos/OctopusGarage/mesh-talk/releases/latest";
  const platforms = {
    macos: {
      name: "macOS",
      title: "Open Mesh-Talk on your Mac",
      targets: { arm64: "Apple silicon", x86_64: "Intel" },
    },
    windows: {
      name: "Windows",
      title: "Install Mesh-Talk on Windows",
      targets: { x86_64: "x64", arm64: "ARM64" },
    },
    linux: {
      name: "Linux",
      title: "Install Mesh-Talk on Linux",
      targets: { x86_64: "x64", aarch64: "ARM64" },
    },
  };
  const primary = document.querySelector("[data-download-primary]");
  const status = document.querySelector("#release-status");
  const retry = document.querySelector("#release-retry");
  const notes = document.querySelector("#release-notes");
  const dialog = document.querySelector(".install-dialog");
  const picker = dialog.querySelector(".install-package-picker");
  const canShowDialog = typeof dialog.showModal === "function";
  let packages = new Map(),
    releaseTag = null,
    opener = null,
    loading = false;
  let architecture = null;

  function detectPlatform() {
    const ua = navigator.userAgent;
    if (
      navigator.userAgentData?.mobile ||
      /Android|iPhone|iPad|iPod/i.test(ua) ||
      (/Macintosh/i.test(ua) && navigator.maxTouchPoints > 1)
    )
      return null;
    const platform = navigator.userAgentData?.platform ?? ua;
    if (/Windows/i.test(platform)) return "windows";
    if (/macOS|Macintosh|MacIntel/i.test(platform)) return "macos";
    if (/Linux/i.test(platform)) return "linux";
    return null;
  }
  const detected = detectPlatform();
  let selectedPlatform = detected;
  let choices = {};
  try {
    const stored = JSON.parse(
      sessionStorage.getItem("mesh-talk-download-choices") ?? "{}",
    );
    if (stored && typeof stored === "object" && !Array.isArray(stored)) {
      for (const os of Object.keys(platforms)) {
        if (Object.hasOwn(platforms[os].targets, stored[os]))
          choices[os] = stored[os];
      }
    }
  } catch {
    /* Preferences are optional when session storage is blocked. */
  }
  function remember(key, asset) {
    selectedPlatform = asset.os;
    choices[asset.os] = key.slice(asset.os.length + 1);
    try {
      sessionStorage.setItem(
        "mesh-talk-download-choices",
        JSON.stringify(choices),
      );
    } catch {
      /* Download remains usable. */
    }
    refreshPrimary();
  }
  if (detected)
    document.dispatchEvent(
      new CustomEvent("mesh-platform-detected", { detail: detected }),
    );

  function refreshPrimary() {
    primary.removeAttribute("data-package");
    primary.href = "#download";
    put(
      primary,
      selectedPlatform
        ? "Download for {platform}"
        : "Choose a desktop download",
      { platform: platforms[selectedPlatform]?.name },
    );
    const selectedArch =
      choices[selectedPlatform] ??
      (selectedPlatform === detected ? architecture : null);
    const key = `${selectedPlatform}_${selectedArch}`;
    const asset = packages.get(key);
    if (asset) {
      primary.href = asset.url;
      primary.dataset.package = key;
      putAttribute(primary, "aria-label", "Download for {platform}", {
        platform: platforms[selectedPlatform].name,
      });
    } else putAttribute(primary, "aria-label", null);
  }
  refreshPrimary();
  document.addEventListener("mesh-platform-selected", (event) => {
    if (!Object.hasOwn(platforms, event.detail)) return;
    selectedPlatform = event.detail;
    refreshPrimary();
  });

  // MacIntel in Safari identifies both Apple silicon and Intel Macs. Do not infer
  // a CPU from it. Use explicit client hints when supplied, otherwise offer both.
  async function detectArchitecture() {
    if (!detected) return;
    let timer;
    try {
      const hints = navigator.userAgentData?.getHighEntropyValues?.([
        "architecture",
        "bitness",
      ]);
      if (!hints) return;
      const values = await Promise.race([
        hints,
        new Promise((resolve) => {
          timer = setTimeout(() => resolve(null), 1200);
        }),
      ]);
      if (values?.bitness === "64") {
        if (values.architecture === "arm")
          architecture = detected === "linux" ? "aarch64" : "arm64";
        if (values.architecture === "x86") architecture = "x86_64";
      }
    } catch {
      /* Missing/denied hints leave an explicit architecture choice. */
    } finally {
      clearTimeout(timer);
      refreshPrimary();
    }
  }
  detectArchitecture();

  function parseRelease(data) {
    if (
      !data ||
      data.draft !== false ||
      data.prerelease !== false ||
      !/^v\d+\.\d+\.\d+$/.test(data.tag_name) ||
      !Array.isArray(data.assets)
    )
      throw Error("Invalid Stable release");
    const tag = data.tag_name;
    const result = new Map();
    for (const [os, platform] of Object.entries(platforms)) {
      for (const [arch, label] of Object.entries(platform.targets)) {
        const key = `${os}_${arch}`;
        const name = `mesh-talk_${tag}_${key}.zip`;
        const matches = data.assets.filter((asset) => asset?.name === name);
        if (matches.length !== 1) continue;
        const asset = matches[0];
        const url = `${releases}/download/${tag}/${name}`;
        if (
          asset.state !== "uploaded" ||
          asset.browser_download_url !== url ||
          !Number.isSafeInteger(asset.size) ||
          asset.size <= 0 ||
          asset.size > 1024 ** 3
        )
          continue;
        result.set(key, {
          url: asset.browser_download_url,
          label,
          size: asset.size,
          os,
        });
      }
    }
    if (!result.size) throw Error("No desktop packages");
    return { tag, packages: result };
  }

  function packageLink(key, asset) {
    const link = document.createElement("a");
    link.className = "package-link";
    link.href = asset.url;
    link.dataset.package = key;
    const label = document.createElement("strong");
    put(label, "Download for {label}", { label: asset.label });
    const detail = document.createElement("span");
    put(detail, "ZIP · {size} MB", {
      size: (asset.size / 1024 / 1024).toFixed(1),
    });
    link.append(label, detail);
    return link;
  }

  async function loadRelease() {
    if (loading) return;
    loading = true;
    retry.hidden = true;
    put(status, "Loading the latest Stable packages…");
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 8000);
    try {
      const response = await fetch(endpoint, {
        signal: controller.signal,
        headers: { Accept: "application/vnd.github+json" },
        credentials: "omit",
        referrerPolicy: "no-referrer",
      });
      if (!response.ok) throw Error("Release lookup failed");
      const selected = parseRelease(await response.json());
      releaseTag = selected.tag;
      packages = selected.packages;
      document.querySelectorAll("[data-packages]").forEach((container) => {
        const os = container.dataset.packages;
        const choices = Object.entries(platforms[os].targets).map(
          ([arch, label]) => {
            const key = `${os}_${arch}`;
            if (packages.has(key)) return packageLink(key, packages.get(key));
            const missing = document.createElement("p");
            missing.className = "package-unavailable";
            put(missing, "{label} is unavailable in this release.", { label });
            return missing;
          },
        );
        container.replaceChildren(...choices);
      });
      notes.href = `${releases}/tag/${releaseTag}`;
      put(notes, "Release notes & verification");
      put(
        status,
        "Stable {version} · Choose your computer’s architecture. Downloads are hosted on GitHub.",
        { version: releaseTag.slice(1) },
      );
      document.querySelectorAll("[data-verification-link]").forEach((link) => {
        link.href = notes.href;
      });
      refreshPrimary();
    } catch {
      put(
        status,
        "We couldn’t load direct downloads. Try again or browse the Stable release on GitHub.",
      );
      retry.hidden = false;
    } finally {
      clearTimeout(timeout);
      loading = false;
    }
  }
  retry.addEventListener("click", loadRelease);
  loadRelease();

  function showGuide(
    os,
    source,
    requested = false,
    choose = false,
    asset = null,
  ) {
    if (!canShowDialog) return;
    document.querySelectorAll("[data-install-guide]").forEach((guide) => {
      guide.hidden = guide.dataset.installGuide !== os;
    });
    put(dialog.querySelector("#install-title"), platforms[os].title);
    put(
      dialog.querySelector("#install-download-status"),
      requested
        ? "Download requested. Check your browser’s downloads, then follow these steps."
        : choose
          ? "Choose your package, then follow these steps."
          : "Follow these steps after downloading the package.",
    );
    put(dialog.querySelector(".copy-feedback"), "");
    picker.hidden = !choose;
    put(
      picker.querySelector("p"),
      architecture
        ? "The matching package is unavailable. Choose a different architecture only if it matches your computer."
        : "Your browser cannot reliably identify your chip. Choose the matching package:",
    );
    if (choose)
      picker
        .querySelector(".package-options")
        .replaceChildren(
          ...[...packages]
            .filter(([, asset]) => asset.os === os)
            .map(([key, asset]) => packageLink(key, asset)),
        );
    const info = dialog.querySelector("[data-download-info]");
    info.hidden = !asset;
    if (asset)
      put(info, "Stable {version} · {platform} · {label} · {size} MB ZIP", {
        version: releaseTag.slice(1),
        platform: platforms[os].name,
        label: asset.label,
        size: (asset.size / 1024 / 1024).toFixed(1),
      });
    const again = dialog.querySelector("[data-download-again]");
    again.hidden = !asset;
    if (asset) {
      again.href = asset.url;
      again.dataset.package = `${asset.os}_${choices[asset.os]}`;
    }
    if (!dialog.open) {
      opener = source;
      dialog.showModal();
    } else if (requested) {
      // Selecting a chip hides the chooser: move focus to a stable action.
      again.focus({ preventScroll: true });
    }
  }

  document.querySelectorAll("[data-install]").forEach((button) => {
    button.hidden = !canShowDialog;
    button.addEventListener("click", () =>
      showGuide(button.dataset.install, button),
    );
  });
  document.addEventListener("click", (event) => {
    const link = event.target.closest("a[data-package]");
    if (
      !link ||
      event.button !== 0 ||
      event.metaKey ||
      event.ctrlKey ||
      event.shiftKey ||
      event.altKey
    )
      return;
    const asset = packages.get(link.dataset.package);
    if (asset) {
      remember(link.dataset.package, asset);
      showGuide(asset.os, link, true, false, asset);
    }
    // Keep the native link action: GitHub serves the ZIP as an attachment. The
    // page never buffers the archive or claims download/verification completed.
  });
  primary.addEventListener("click", (event) => {
    if (
      primary.hasAttribute("data-package") ||
      !selectedPlatform ||
      !canShowDialog ||
      event.button !== 0 ||
      event.metaKey ||
      event.ctrlKey ||
      event.shiftKey ||
      event.altKey
    )
      return;
    if (![...packages.values()].some((asset) => asset.os === selectedPlatform))
      return;
    event.preventDefault();
    showGuide(selectedPlatform, primary, false, true);
  });
  dialog.addEventListener("close", () => opener?.focus());
  dialog.addEventListener("click", (event) => {
    if (event.target !== dialog) return;
    const rect = dialog.getBoundingClientRect();
    if (
      event.clientX < rect.left ||
      event.clientX > rect.right ||
      event.clientY < rect.top ||
      event.clientY > rect.bottom
    )
      dialog.close();
  });
})();
