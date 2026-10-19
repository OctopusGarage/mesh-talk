export const escapeHtml = (value) =>
  String(value).replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ],
  );
function inline(text) {
  const pattern = /(`[^`]+`|\*\*[^*]+\*\*|\[[^\]]+\]\([^\s)]+\))/g;
  let result = "",
    last = 0;
  for (const match of text.matchAll(pattern)) {
    result += escapeHtml(text.slice(last, match.index));
    const token = match[0];
    if (token.startsWith("`"))
      result += "<code>" + escapeHtml(token.slice(1, -1)) + "</code>";
    else if (token.startsWith("**"))
      result += "<strong>" + escapeHtml(token.slice(2, -2)) + "</strong>";
    else {
      const [, label, url] = token.match(/^\[([^\]]+)\]\(([^)]+)\)$/);
      const safe =
        /^https:\/\/[^\s<>"']+$/i.test(url) ||
        /^[a-z0-9][a-z0-9-]*\.md(?:#[a-z0-9-]+)?$/.test(url) ||
        /^#[a-z0-9-]+$/.test(url);
      result += safe
        ? '<a href="' + escapeHtml(url) + '">' + escapeHtml(label) + "</a>"
        : escapeHtml(label);
    }
    last = match.index + token.length;
  }
  return result + escapeHtml(text.slice(last));
}
/** Restricted Markdown: raw HTML is always text; only HTTPS and topic links are allowed. */
export function renderMarkdown(body) {
  let html = "",
    list = null,
    paragraph = [];
  const flush = () => {
    if (paragraph.length) {
      html += "<p>" + inline(paragraph.join(" ")) + "</p>\n";
      paragraph = [];
    }
  };
  const close = () => {
    if (list) {
      html += "</" + list + ">\n";
      list = null;
    }
  };
  for (const line of body.replace(/\r\n/g, "\n").split("\n")) {
    const heading = line.match(/^(#{1,3})\s+(.+)$/),
      item = line.match(/^\s*(?:(\d+)\.|[-*])\s+(.+)$/);
    if (heading) {
      flush();
      close();
      const level = heading[1].length;
      html += `<h${level}>${inline(heading[2])}</h${level}>\n`;
    } else if (item) {
      flush();
      const type = item[1] ? "ol" : "ul";
      if (list !== type) {
        close();
        list = type;
        html += "<" + type + ">\n";
      }
      html += "<li>" + inline(item[2]) + "</li>\n";
    } else if (!line.trim()) {
      flush();
      close();
    } else {
      close();
      paragraph.push(line);
    }
  }
  flush();
  close();
  return html;
}
