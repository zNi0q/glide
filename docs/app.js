const root = document.documentElement;
const themeButton = document.querySelector("[data-theme-toggle]");

function storedTheme() {
  try {
    return localStorage.getItem("glide-theme");
  } catch {
    return null;
  }
}

function currentTheme() {
  return root.dataset.theme || (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
}

function applyTheme(theme) {
  root.dataset.theme = theme;
  themeButton?.setAttribute("aria-label", theme === "dark" ? "Switch to light theme" : "Switch to dark theme");
}

const saved = storedTheme();
if (saved === "light" || saved === "dark") {
  applyTheme(saved);
}

themeButton?.addEventListener("click", () => {
  const next = currentTheme() === "dark" ? "light" : "dark";
  applyTheme(next);
  try {
    localStorage.setItem("glide-theme", next);
  } catch {}
});

const copyIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>';
const doneIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>';

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.append(area);
    area.select();
    const copied = document.execCommand("copy");
    area.remove();
    return copied;
  }
}

for (const block of document.querySelectorAll(".code")) {
  const button = document.createElement("button");
  button.className = "copy";
  button.type = "button";
  button.setAttribute("aria-label", "Copy to clipboard");
  button.innerHTML = copyIcon;
  button.addEventListener("click", async () => {
    const lines = [...block.querySelectorAll("pre code")]
      .map((code) => code.innerText)
      .join("\n")
      .split("\n")
      .filter((line) => !line.trimStart().startsWith("#"))
      .map((line) => line.replace(/^\$ /, ""));
    if (await copyText(lines.join("\n").trim())) {
      button.dataset.copied = "true";
      button.innerHTML = doneIcon;
      button.setAttribute("aria-label", "Copied");
      setTimeout(() => {
        button.dataset.copied = "false";
        button.innerHTML = copyIcon;
        button.setAttribute("aria-label", "Copy to clipboard");
      }, 1600);
    }
  });
  block.append(button);
}

const links = new Map(
  [...document.querySelectorAll(".sidebar a")].map((link) => [link.getAttribute("href").slice(1), link])
);
const visible = new Set();
const observer = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      if (entry.isIntersecting) {
        visible.add(entry.target.id);
      } else {
        visible.delete(entry.target.id);
      }
    }
    const active = [...links.keys()].find((id) => visible.has(id));
    for (const [id, link] of links) {
      link.setAttribute("aria-current", String(id === active));
    }
  },
  { rootMargin: "-80px 0px -55% 0px" }
);
for (const id of links.keys()) {
  const section = document.getElementById(id);
  if (section) {
    observer.observe(section);
  }
}

for (const link of document.querySelectorAll(".mobile-toc a")) {
  link.addEventListener("click", () => link.closest("details")?.removeAttribute("open"));
}

const versionBadge = document.querySelector("[data-version]");
if (versionBadge) {
  fetch("https://api.github.com/repos/zNi0q/glide/releases/latest")
    .then((response) => (response.ok ? response.json() : null))
    .then((release) => {
      if (release?.tag_name) {
        versionBadge.textContent = release.tag_name;
      }
    })
    .catch(() => {});
}
