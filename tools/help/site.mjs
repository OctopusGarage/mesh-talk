import { searchHelp } from "./search.mjs";
const input = document.querySelector("#search"),
  results = document.querySelector("#results"),
  status = document.querySelector("#status");
try {
  const response = await fetch("./index.json");
  if (!response.ok) throw Error("Unavailable");
  const { topics } = await response.json();
  const update = () => {
    const found = searchHelp(topics, input.value);
    results.replaceChildren(
      ...found.map((t) => {
        const li = document.createElement("li"),
          a = document.createElement("a"),
          p = document.createElement("p");
        a.href = t.id + ".html";
        a.textContent = t.title;
        p.textContent = t.summary;
        li.append(a, p);
        return li;
      }),
    );
    status.textContent = found.length
      ? found.length + " topics"
      : "No topics match. Try a shorter phrase or browse topics.";
  };
  input.addEventListener("input", update);
  update();
} catch {
  status.textContent =
    "Search is unavailable. You can still browse the topics below.";
}
