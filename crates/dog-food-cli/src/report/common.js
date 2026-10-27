/* Shared by every dog-food page: helpers, the theme switch and the tooltip. */
const $ = id => document.getElementById(id);
const esc = s => String(s).replace(/[&<>"]/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const plural = (n, word) => `${Number(n).toLocaleString()} ${word}${n === 1 ? "" : "s"}`;
/* Within limits uses the accent, ignored is quiet, warn and fail use their status colors. */
const color = status => `var(--${{ ok: "accent", ignored: "faint" }[status] || status})`;
const signed = n => (n > 0 ? `+${n}` : n < 0 ? `−${-n}` : "±0");

/* Round axis steps (1, 2, 2.5, 5 × 10ⁿ), never below 1. */
function niceTicks(max, count) {
  const raw = Math.max(max, 1) / count;
  const power = Math.pow(10, Math.floor(Math.log10(raw)));
  const step = Math.max(1, [1, 2, 2.5, 5, 10].map(m => m * power).find(s => s >= raw));
  const top = Math.ceil(Math.max(max, 1) / step) * step;
  return Array.from({ length: Math.round(top / step) + 1 }, (_, i) => Math.round(i * step * 100) / 100);
}
const svg = (w, h, label, body) =>
  `<svg viewBox="0 0 ${w} ${h}" role="img" aria-label="${esc(label)}">${body}</svg>`;
const limitLine = (x1, y1, x2, y2, status, label, anchor) =>
  `<line x1="${x1}" y1="${y1}" x2="${x2}" y2="${y2}" stroke="${color(status)}" stroke-width="1.5" stroke-dasharray="5 4"/>` +
  `<text x="${anchor.x}" y="${anchor.y}" text-anchor="${anchor.align}" font-size="11" font-weight="700" fill="${color(status)}">${label}</text>`;

/* "fail" / "warn" / "pass" card in the header. */
function verdictCard(state, title, text) {
  const mark = { fail: "!", warn: "~", pass: "✓" }[state];
  $("verdict").className = `verdict ${state}`;
  $("verdict").innerHTML = `<div class="badge" aria-hidden="true">${mark}</div><div><strong>${esc(title)}</strong><span>${esc(text)}</span></div>`;
}

/* [value, unit, label, class] tiles. */
function kpiStrip(items) {
  $("kpis").style.gridTemplateColumns = `repeat(${items.length}, minmax(0, 1fr))`;
  $("kpis").innerHTML = items.map(([v, unit, label, cls]) =>
    `<div class="kpi ${cls || ""}"><span class="v">${v}<small>${unit || ""}</small></span><span class="k">${label}</span></div>`).join("");
}

(function themeSwitch() {
  const root = document.documentElement;
  const set = t => { if (t) root.dataset.theme = t; else delete root.dataset.theme; };
  try { set(localStorage.getItem("dgf-theme")); } catch (e) {}
  const button = $("theme");
  if (!button) return;
  button.addEventListener("click", () => {
    const dark = root.dataset.theme ? root.dataset.theme === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
    const next = dark ? "light" : "dark";
    set(next);
    try { localStorage.setItem("dgf-theme", next); } catch (e) {}
  });
})();

/* Any element with data-tip shows it in a tooltip that follows the mouse. */
(function tooltip() {
  const tip = $("tip");
  if (!tip) return;
  document.addEventListener("mouseover", e => {
    const target = e.target.closest && e.target.closest("[data-tip]");
    if (!target) { tip.hidden = true; return; }
    tip.textContent = target.getAttribute("data-tip");
    tip.hidden = false;
  });
  document.addEventListener("mousemove", e => {
    if (tip.hidden) return;
    const pad = 14, w = tip.offsetWidth, h = tip.offsetHeight;
    const x = e.clientX + pad + w > innerWidth ? e.clientX - pad - w : e.clientX + pad;
    const y = e.clientY + pad + h > innerHeight ? e.clientY - pad - h : e.clientY + pad;
    tip.style.left = `${x}px`; tip.style.top = `${y}px`;
  });
})();
