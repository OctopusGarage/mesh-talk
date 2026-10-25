const navigation = document.querySelector("aside details");
const wide = window.matchMedia("(min-width: 721px)");
function adapt() {
  navigation.open = wide.matches;
}
adapt();
wide.addEventListener("change", adapt);

// A single shortcut works in both the index and articles without stealing keys
// from editable controls or modified browser shortcuts.
document.addEventListener("keydown", (event) => {
  if (
    event.key !== "/" ||
    event.ctrlKey ||
    event.metaKey ||
    event.altKey ||
    event.target.closest("input,textarea,select,[contenteditable]")
  )
    return;
  event.preventDefault();
  const search = document.querySelector("#search");
  if (search) search.focus();
  else location.href = "index.html#search";
});
if (location.hash === "#search") document.querySelector("#search")?.focus();

const contents = [
  ...document.querySelectorAll('.article-contents a[href^="#"]'),
];
if (contents.length && "IntersectionObserver" in window) {
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        contents.forEach((link) => {
          if (link.hash === `#${entry.target.id}`)
            link.setAttribute("aria-current", "location");
          else link.removeAttribute("aria-current");
        });
      }
    },
    { rootMargin: "-10% 0px -65% 0px" },
  );
  contents.forEach((link) => {
    const heading = document.getElementById(link.hash.slice(1));
    if (heading) observer.observe(heading);
  });
}
