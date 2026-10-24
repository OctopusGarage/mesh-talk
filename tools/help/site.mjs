import { searchHelp } from "./search.mjs";
const input = document.querySelector("#search"),
  results = document.querySelector("#results"),
  status = document.querySelector("#status");
const clear = document.querySelector("#search-clear"),
  retry = document.querySelector("#search-retry");
const put = (element, source, values = {}) => {
  if (window.MeshTalkLocale)
    window.MeshTalkLocale.text(element, source, values);
  else
    element.textContent = source.replace(
      /\{(\w+)\}/g,
      (_, key) => values[key] ?? `{${key}}`,
    );
};
let topics = null,
  loading = false;
function readQuery() {
  input.value = new URL(location.href).searchParams.get("q") ?? "";
}
function update(persist = true) {
  clear.hidden = !input.value;
  if (persist) {
    const url = new URL(location.href);
    if (input.value) url.searchParams.set("q", input.value);
    else url.searchParams.delete("q");
    history.replaceState(null, "", url);
  }
  if (!topics) return;
  const found = searchHelp(topics, input.value);
  results.replaceChildren(
    ...found.map((topic) => {
      const li = document.createElement("li"),
        a = document.createElement("a"),
        p = document.createElement("p");
      a.href = topic.id + ".html";
      a.textContent = topic.title;
      p.textContent = topic.summary;
      li.append(a, p);
      return li;
    }),
  );
  put(
    status,
    found.length
      ? "{count} topics"
      : "No topics match. Try a shorter phrase or browse topics.",
    { count: found.length },
  );
}
async function load() {
  if (loading) return;
  loading = true;
  retry.hidden = true;
  put(status, "Loading search. You can browse all topics below.");
  const controller = new AbortController(),
    timeout = setTimeout(() => controller.abort(), 8000);
  try {
    const response = await fetch("./index.json", { signal: controller.signal });
    if (!response.ok) throw Error("Unavailable");
    const data = await response.json();
    if (
      !Array.isArray(data.topics) ||
      !data.topics.every(
        (t) =>
          t &&
          /^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(t.id) &&
          [t.title, t.summary, t.body].every((x) => typeof x === "string") &&
          Array.isArray(t.keywords),
      )
    )
      throw Error("Invalid index");
    topics = data.topics;
    update(false);
  } catch {
    put(
      status,
      "Search is unavailable. You can still browse the topics below.",
    );
    retry.hidden = false;
  } finally {
    clearTimeout(timeout);
    loading = false;
  }
}
readQuery();
update(false);
input.addEventListener("input", () => update());
input.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && input.value) {
    event.preventDefault();
    input.value = "";
    update();
  }
});
clear.addEventListener("click", () => {
  input.value = "";
  update();
  input.focus();
});
retry.addEventListener("click", load);
document.querySelector("#help-search").addEventListener("submit", (event) => {
  event.preventDefault();
  update();
});
addEventListener("popstate", () => {
  readQuery();
  update(false);
});
addEventListener("pageshow", () => {
  readQuery();
  update(false);
});
load();
