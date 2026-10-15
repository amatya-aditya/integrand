"use strict";
// Integrand — main window. Vanilla JS over Tauri commands (see src/main.rs).

const T = window.__TAURI__;
const invoke = T.core.invoke;
const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

const S = {
  settings: null, status: null, results: [], history: [], paused: false, version: "", dataDir: "", platform: "windows",
  page: "history", sel: null, origColors: (() => { try { return !!localStorage.getItem("origColors"); } catch { return false; } })(), q: "", filter: "all", recording: null, candidates: null, apps: [],
};

const SAMPLE = String.raw`\int_0^\infty e^{-x^2}\,dx=\frac{\sqrt{\pi}}{2}`;
const ACTIONS = {
  snipPaste: "Snip and paste",
  clipboardPaste: "Paste clipboard image",
  snipCopy: "Snip and copy",
  clipboardCopy: "Copy clipboard image",
  openHistory: "Open history",
};
const ICON = {
  star: '<svg viewBox="0 0 24 24"><path d="m12 3.6 2.6 5.3 5.8.8-4.2 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8-4.2-4.1 5.8-.8z"/></svg>',
  rerun: '<svg viewBox="0 0 24 24"><path d="M4 12a8 8 0 0 1 13.7-5.6L20 8.7M20 4v4.7h-4.7M20 12a8 8 0 0 1-13.7 5.6L4 15.3M4 20v-4.7h4.7"/></svg>',
  trash: '<svg viewBox="0 0 24 24"><path d="M4.5 7h15M10 11v6M14 11v6M6 7l1 12.5h10L18 7M9 7V4.5h6V7"/></svg>',
  copy: '<svg viewBox="0 0 24 24"><rect x="8.5" y="8.5" width="11" height="11" rx="2"/><path d="M15.5 8.5V6a1.5 1.5 0 0 0-1.5-1.5H6A1.5 1.5 0 0 0 4.5 6v8A1.5 1.5 0 0 0 6 15.5h2.5"/></svg>',
  search: '<svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.4-4.4"/></svg>',
  plus: '<svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>',
  x: '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6 6 18"/></svg>',
  check: '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2.2"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>',
  warn: '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 9v4M12 17h.01M10.3 3.9 2.4 18a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z"/></svg>',
};

// ------------------------------------------------------------ helpers

function tex(el, latex, displayMode = true) {
  try {
    katex.render(latex || "\\,", el, { displayMode, throwOnError: false, strict: "ignore", trust: false, output: "html" });
  } catch {
    el.textContent = latex;
  }
}

/** Render display math and shrink it until it fits its box (long equations). */
function texFit(el, latex) {
  el.style.fontSize = "";
  tex(el, latex);
  const box = el.parentElement;
  const avail = box.clientWidth - 68;
  let size = parseFloat(getComputedStyle(el).fontSize);
  while (el.scrollWidth > avail && size > 12) {
    size -= 1;
    el.style.fontSize = size + "px";
  }
}

function renderTemplate(template, latex) {
  return template
    .replace(/\{mathml\}/g, '<math xmlns="http://www.w3.org/1998/Math/MathML">…</math>')
    .replace(/\{latex_wl_str\}/g, latex.replace(/\\/g, "\\\\").replace(/"/g, '\\"'))
    .replace(/\{latex_wl\}/g, latex)
    .replace(/\{latex_clean\}/g, latex)
    .replace(/\{latex_str\}/g, latex.replace(/\\/g, "\\\\").replace(/"/g, '\\"'))
    .replace(/\{latex_sq\}/g, latex.replace(/'/g, "''"))
    .replace(/\{latex\}/g, latex);
}

let toastTimer;
function toast(msg, bad = false) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.toggle("bad", bad);
  t.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("show"), bad ? 4200 : 1800);
}

function keyLabel(tok) {
  const mac = S.platform === "macos";
  const map = { Ctrl: mac ? "⌃" : "Ctrl", Control: "Ctrl", Alt: mac ? "⌥" : "Alt", Shift: mac ? "⇧" : "Shift", Super: mac ? "⌘" : "Win", Cmd: "⌘",
    ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→", Backquote: "`", Minus: "-", Equal: "=", BracketLeft: "[",
    BracketRight: "]", Backslash: "\\", Semicolon: ";", Quote: "'", Comma: ",", Period: ".", Slash: "/", PrintScreen: "PrtSc" };
  if (map[tok]) return map[tok];
  if (/^Key[A-Z]$/.test(tok)) return tok.slice(3);
  if (/^Digit\d$/.test(tok)) return tok.slice(5);
  if (/^Numpad/.test(tok)) return "Num " + tok.slice(6);
  return tok;
}
function keycaps(keys) {
  if (!keys) return '<span class="empty">Not set</span>';
  return '<span class="keys">' + keys.split("+").map((k) => `<kbd>${esc(keyLabel(k.trim()))}</kbd>`).join('<span class="plus">+</span>') + "</span>";
}
const hotkeyFor = (action) => S.settings.hotkeys.find((h) => h.action === action && h.enabled && h.keys)?.keys;

function appColor(name) {
  let h = 0;
  for (const c of name.toLowerCase()) h = (h * 31 + c.charCodeAt(0)) % 360;
  return `hsl(${h} 52% 46%)`;
}
function initials(name) {
  const w = name.replace(/\.exe$/i, "").split(/[\s_-]+/).filter(Boolean);
  return ((w[0]?.[0] || "?") + (w[1]?.[0] || w[0]?.[1] || "")).toUpperCase();
}
function prettyExe(exe) {
  const s = exe.replace(/\.exe$/i, "");
  return s.charAt(0).toUpperCase() + s.slice(1).toLowerCase();
}
const fmtName = (id) => (id === "auto" ? "Match the app" : S.settings.formats.find((f) => f.id === id)?.name ?? id);

function timeOf(iso) {
  return new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}
function dayOf(iso) {
  const d = new Date(iso), today = new Date();
  const y = new Date(); y.setDate(today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === y.toDateString()) return "Yesterday";
  return d.toLocaleDateString([], { weekday: "long", month: "short", day: "numeric", year: d.getFullYear() === today.getFullYear() ? undefined : "numeric" });
}

// Program icons (exe → data URL), loaded from the backend on demand.
const ICONS = {};
const iconWanted = new Set();
async function loadIcons(apps) {
  const todo = apps.filter(([exe]) => exe && !(exe.toLowerCase() in ICONS) && !iconWanted.has(exe.toLowerCase()));
  if (!todo.length) return false;
  todo.forEach(([exe]) => iconWanted.add(exe.toLowerCase()));
  const got = await invoke("app_icons", { apps: todo }).catch(() => ({}));
  for (const [exe] of todo) ICONS[exe.toLowerCase()] = got[exe] || null;
  return Object.keys(got).length > 0;
}
const iconFor = (exe) => (exe ? ICONS[exe.toLowerCase()] : null);
function appIcon(exe, name, cls = "app-badge") {
  const url = iconFor(exe);
  return url
    ? `<img class="${cls} img" src="${url}" alt="">`
    : `<span class="${cls}" style="background:${appColor(name || exe)}">${esc(initials(name || exe))}</span>`;
}

let saveTimer;
function persist(immediate = false) {
  clearTimeout(saveTimer);
  const run = async () => {
    try {
      S.results = await invoke("save_settings", { settings: S.settings });
      if (S.page === "hotkeys") updateHotkeyStatus();
    } catch (e) {
      toast(String(e), true);
    }
  };
  immediate ? run() : (saveTimer = setTimeout(run, 350));
}

const systemDark = matchMedia("(prefers-color-scheme: dark)");
function applyTheme() {
  const t = S.settings.theme;
  if (t === "light" || t === "dark") document.documentElement.dataset.theme = t;
  else delete document.documentElement.dataset.theme;
  // Native title bar takes the sidebar colour so the frame reads as part of the app.
  const css = getComputedStyle(document.documentElement);
  const v = (name) => css.getPropertyValue(name).trim();
  invoke("set_titlebar", {
    caption: v("--surface"), text: v("--ink"), border: v("--line"),
    dark: t === "dark" || (t !== "light" && systemDark.matches),
  }).catch(() => {});
}
systemDark.addEventListener("change", () => S.settings && applyTheme());

// ------------------------------------------------------------ navigation

function go(page) {
  if (S.recording) stopRecording(false);
  S.page = page;
  $$(".nav button").forEach((b) => (b.dataset.page === page ? b.setAttribute("aria-current", "page") : b.removeAttribute("aria-current")));
  $$(".page").forEach((p) => p.classList.toggle("active", p.id === "page-" + page));
  RENDER[page]();
}

// ------------------------------------------------------------ status pill

function renderStatus() {
  const s = S.status;
  if (!s) return;
  $("#engine-dot").className = "dot " + s.state;
  const label = { ready: "Ready", busy: "Recognizing", snipping: "Select an area", loading: "Starting", offline: "Offline", error: "Needs attention", setup: "Setup needed" }[s.state] || s.state;
  $("#engine-state").textContent = S.paused ? "Hotkeys paused" : label;
  $("#engine-sub").textContent = s.state === "snipping" ? "Press Esc to cancel" : s.state === "ready"
    ? [s.device ? s.device.toUpperCase() : "", s.lastMs != null ? `${s.lastMs} ms last` : ""].filter(Boolean).join(", ") || s.message
    : s.message;
  if (S.page === "engine") renderEngineCard();
}

// ------------------------------------------------------------ history

function filteredHistory() {
  const q = S.q.trim().toLowerCase();
  return S.history.filter((e) =>
    (S.filter !== "star" || e.favorite) &&
    (!q || e.latex.toLowerCase().includes(q) || (e.appName || "").toLowerCase().includes(q) || (e.app || "").toLowerCase().includes(q)));
}

function renderHistory() {
  const page = $("#page-history");
  if (!page.dataset.built) {
    page.dataset.built = "1";
    page.innerHTML = `
      <div class="list-pane">
        <div class="list-head">
          <div class="top"><h1>History</h1>
            <button class="btn" id="h-clip" title="Convert the image on the clipboard">${ICON.plus}From clipboard</button></div>
          <div class="search">${ICON.search}<input class="field" id="h-q" placeholder="Search LaTeX or app" spellcheck="false"></div>
          <div class="filters">
            <button class="chip" data-f="all" aria-pressed="true">All</button>
            <button class="chip" data-f="star" aria-pressed="false">Starred</button>
          </div>
        </div>
        <div class="list" id="h-list" role="listbox" aria-label="Conversions" tabindex="0"></div>
      </div>
      <div class="detail" id="h-detail"></div>`;
    $("#h-q").addEventListener("input", (e) => { S.q = e.target.value; renderList(); });
    $$(".filters .chip").forEach((c) => c.addEventListener("click", () => {
      S.filter = c.dataset.f;
      $$(".filters .chip").forEach((x) => x.setAttribute("aria-pressed", String(x === c)));
      renderList();
    }));
    $("#h-clip").addEventListener("click", convertClipboard);
    $("#h-list").addEventListener("keydown", (e) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      e.preventDefault();
      const items = filteredHistory();
      const i = items.findIndex((x) => x.id === S.sel);
      const next = items[Math.max(0, Math.min(items.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))];
      if (next) select(next.id, true);
    });
  }
  renderList();
}

function renderList() {
  const list = $("#h-list");
  const items = filteredHistory();
  if (!S.history.length) {
    list.innerHTML = "";
    renderEmpty();
    return;
  }
  if (!items.find((e) => e.id === S.sel)) S.sel = items[0]?.id ?? null;
  let html = "", day = "";
  for (const e of items) {
    const d = dayOf(e.time);
    if (d !== day) { html += `<div class="day">${esc(d)}</div>`; day = d; }
    html += `<button class="item" role="option" data-id="${esc(e.id)}" aria-selected="${e.id === S.sel}">
      ${e.favorite ? `<svg class="star" viewBox="0 0 24 24"><path d="m12 3.6 2.6 5.3 5.8.8-4.2 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8-4.2-4.1 5.8-.8z"/></svg>` : ""}
      <div class="eq" data-tex="${esc(e.latex)}"></div>
      <div class="meta"><span class="where">${e.app && iconFor(e.app) ? `<img src="${iconFor(e.app)}" alt="">` : ""}${esc(e.appName ? `Pasted into ${e.appName}` : "Copied")}</span><span>${timeOf(e.time)}</span></div>
    </button>`;
  }
  list.innerHTML = html || `<div class="day">No matches for “${esc(S.q)}”</div>`;
  $$(".eq", list).forEach((el) => tex(el, el.dataset.tex, false));
  const apps = [...new Set(items.map((e) => e.app).filter(Boolean))].map((a) => [a, ""]);
  loadIcons(apps).then((changed) => changed && S.page === "history" && renderList());
  $$(".item", list).forEach((b) => b.addEventListener("click", () => select(b.dataset.id)));
  renderDetail();
}

function select(id, scroll = false) {
  S.sel = id;
  $$("#h-list .item").forEach((b) => b.setAttribute("aria-selected", String(b.dataset.id === id)));
  if (scroll) $(`#h-list .item[data-id="${CSS.escape(id)}"]`)?.scrollIntoView({ block: "nearest" });
  renderDetail();
}

function renderEmpty() {
  const snip = hotkeyFor("snipPaste"), clip = hotkeyFor("clipboardPaste");
  $("#h-detail").innerHTML = `
    <div class="empty-state">
      <img src="logo.svg" alt="">
      <h2>Snip an equation to get started</h2>
      <p>Works in Word, PowerPoint, Mathematica, MATLAB, your editor, anywhere you can type.</p>
      <div class="how">
        <div><span class="n">1</span><span>Click where the math should go, then press ${keycaps(snip)}</span></div>
        <div><span class="n">2</span><span>Drag a box around the equation</span></div>
        <div><span class="n">3</span><span>It’s pasted at your cursor, formatted for that app</span></div>
      </div>
      <p class="drop-hint">Already copied an image? Press ${keycaps(clip)}. You can also drop image files here.</p>
    </div>`;
}

function renderDetail() {
  const box = $("#h-detail");
  const e = S.history.find((x) => x.id === S.sel);
  if (!e) {
    if (S.history.length) box.innerHTML = `<div class="empty-state"><p>Nothing selected.</p></div>`;
    else renderEmpty();
    return;
  }
  const img = e.image ? T.core.convertFileSrc(e.image) : null;
  const formats = S.settings.formats.map((f) =>
    `<button class="fmt" data-fmt="${esc(f.id)}" title="Copy as ${esc(f.name)}"><b>${esc(f.name)}</b><code>${esc(renderTemplate(f.template, e.latex))}</code></button>`).join("");
  const when = new Date(e.time).toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
  box.innerHTML = `
    <div class="specimen">
      <div class="math" id="d-math"></div>
      <div class="tools">
        <button class="icon-btn ${e.favorite ? "on" : ""}" id="d-star" title="${e.favorite ? "Unstar" : "Star"}" aria-pressed="${e.favorite}">${ICON.star}</button>
        <button class="icon-btn" id="d-rerun" title="Run recognition again" ${e.image ? "" : "disabled"}>${ICON.rerun}</button>
        <button class="icon-btn" id="d-del" title="Delete">${ICON.trash}</button>
      </div>
    </div>
    <div class="compare">
      <div class="panel"><header>Source${img ? `<button class="btn ghost dark-only" id="d-orig" aria-pressed="${!!S.origColors}">${S.origColors ? "Match theme" : "Original colours"}</button>` : ""}</header>
        <div class="src ${S.origColors ? "orig" : ""}">${img ? `<img src="${esc(img)}" alt="Source image">` : '<span class="hint">Image not kept</span>'}</div></div>
      <div class="panel"><header>LaTeX<button class="btn ghost" id="d-copy">${ICON.copy}Copy</button></header>
        <textarea id="d-tex" spellcheck="false" aria-label="LaTeX source">${esc(e.latex)}</textarea></div>
    </div>
    <div class="copy-as"><h3>Copy as</h3><div class="formats">${formats}</div></div>
    <div class="facts">
      <span><b>${esc(when)}</b></span>
      <span>${e.appName ? `Pasted into <b>${esc(e.appName)}</b>` : "Copied to clipboard"}</span>
      <span>Format <b>${esc(fmtName(e.format))}</b></span>
      <span>Recognized in <b>${e.ms} ms</b></span>
    </div>`;
  texFit($("#d-math"), e.latex);
  let editTimer;
  $("#d-tex").addEventListener("input", (ev) => {
    e.latex = ev.target.value;
    texFit($("#d-math"), e.latex);
    $$(".fmt", box).forEach((b) => { b.querySelector("code").textContent = renderTemplate(S.settings.formats.find((f) => f.id === b.dataset.fmt).template, e.latex); });
    const li = $(`#h-list .item[data-id="${CSS.escape(e.id)}"] .eq`);
    if (li) tex(li, e.latex, false);
    clearTimeout(editTimer);
    editTimer = setTimeout(() => invoke("history_update", { id: e.id, latex: e.latex }), 500);
  });
  $("#d-copy").addEventListener("click", () => copyAs(e, "latex"));
  $("#d-orig")?.addEventListener("click", () => {
    S.origColors = !S.origColors;
    try { localStorage.setItem("origColors", S.origColors ? "1" : ""); } catch {}
    renderDetail();
  });
  $$(".fmt", box).forEach((b) => b.addEventListener("click", () => copyAs(e, b.dataset.fmt, b)));
  $("#d-star").addEventListener("click", async () => {
    e.favorite = !e.favorite;
    await invoke("history_favorite", { id: e.id, favorite: e.favorite });
    renderList();
  });
  $("#d-del").addEventListener("click", async () => {
    const items = filteredHistory();
    const i = items.findIndex((x) => x.id === e.id);
    await invoke("history_delete", { id: e.id });
    S.history = S.history.filter((x) => x.id !== e.id);
    S.sel = (items[i + 1] || items[i - 1])?.id ?? null;
    renderList();
    toast("Deleted");
  });
  $("#d-rerun").addEventListener("click", async (ev) => {
    const btn = ev.currentTarget;
    btn.disabled = true;
    try {
      const updated = await invoke("history_rerun", { id: e.id });
      Object.assign(e, updated);
      renderList();
      toast(`Recognized again in ${updated.ms} ms`);
    } catch (err) {
      toast(String(err), true);
    } finally {
      btn.disabled = false;
    }
  });
}

async function copyAs(entry, format, btn) {
  try {
    await invoke("copy_formatted", { latex: entry.latex, format });
    toast(`Copied as ${fmtName(format)}`);
    if (btn) { btn.classList.add("done"); setTimeout(() => btn.classList.remove("done"), 1200); }
  } catch (e) {
    toast(String(e), true);
  }
}

async function convertClipboard() {
  const b = $("#h-clip");
  if (b) b.disabled = true;
  try {
    const e = await invoke("ocr_clipboard");
    toast(`Converted and copied (${e.ms} ms)`);
  } catch (e) {
    toast(String(e), true);
  } finally {
    if (b) b.disabled = false;
  }
}

// ------------------------------------------------------------ hotkeys

function renderHotkeys() {
  const page = $("#page-hotkeys");
  const fmtOptions = (sel) => `<option value="auto" ${sel === "auto" ? "selected" : ""}>Match the app</option>` +
    S.settings.formats.map((f) => `<option value="${esc(f.id)}" ${sel === f.id ? "selected" : ""}>${esc(f.name)}</option>`).join("");
  const rows = S.settings.hotkeys.map((h, i) => `
    <div class="row stack" data-i="${i}">
      <div class="hk-row">
        <select class="field" data-k="action" aria-label="Action">
          ${Object.entries(ACTIONS).map(([v, l]) => `<option value="${v}" ${h.action === v ? "selected" : ""}>${l}</option>`).join("")}
        </select>
        <button class="recorder" data-k="record" title="Click, then press a new shortcut">${keycaps(h.keys)}</button>
        <select class="field" data-k="format" aria-label="Output format" ${h.action === "openHistory" ? 'style="visibility:hidden"' : ""}>${fmtOptions(h.format)}</select>
        <div style="display:flex;gap:6px;align-items:center">
          <label class="switch" title="${h.enabled ? "Enabled" : "Disabled"}"><input type="checkbox" data-k="enabled" ${h.enabled ? "checked" : ""} aria-label="Enabled"><i></i></label>
          <button class="icon-btn" data-k="remove" title="Remove">${ICON.x}</button>
        </div>
      </div>
      <div class="hk-status" data-status="${esc(h.id)}"></div>
    </div>`).join("");
  page.innerHTML = `<div class="scroll"><div class="narrow">
    <h1>Hotkeys</h1>
    <p class="lede">These work in every app. With <em>Match the app</em>, the result is formatted for the program you’re in, set on the Apps page.</p>
    <div class="group" id="hk-group">${rows || '<div class="row"><span class="hint">No hotkeys. Add one below.</span></div>'}</div>
    <div class="add-bar"><button class="btn" id="hk-add">${ICON.plus}Add hotkey</button></div>
    <p class="hint">Click a shortcut, then press the new combination. Use at least one of Ctrl, Alt, Shift or Win. Esc cancels.</p>
  </div></div>`;
  updateHotkeyStatus();
  $$("#hk-group .row").forEach((row) => {
    const h = S.settings.hotkeys[+row.dataset.i];
    row.querySelector('[data-k="action"]').addEventListener("change", (e) => { h.action = e.target.value; persist(); renderHotkeys(); });
    row.querySelector('[data-k="format"]').addEventListener("change", (e) => { h.format = e.target.value; persist(); });
    row.querySelector('[data-k="enabled"]').addEventListener("change", (e) => { h.enabled = e.target.checked; persist(); });
    row.querySelector('[data-k="remove"]').addEventListener("click", () => {
      S.settings.hotkeys.splice(+row.dataset.i, 1);
      persist(true);
      renderHotkeys();
    });
    row.querySelector('[data-k="record"]').addEventListener("click", (e) => startRecording(h, e.currentTarget));
  });
  $("#hk-add").addEventListener("click", () => {
    const h = { id: "hk" + Date.now(), action: "snipPaste", keys: "", format: "auto", enabled: true };
    S.settings.hotkeys.push(h);
    renderHotkeys();
    startRecording(h, $$("#hk-group .recorder").at(-1));
  });
}

function updateHotkeyStatus() {
  for (const el of $$("[data-status]")) {
    const h = S.settings.hotkeys.find((x) => x.id === el.dataset.status);
    const r = S.results.find((x) => x.id === el.dataset.status);
    if (!h?.enabled) el.innerHTML = `<span class="reg">Off</span>`;
    else if (!h.keys) el.innerHTML = `<span class="reg bad">${ICON.warn}No shortcut set</span>`;
    else if (S.paused) el.innerHTML = `<span class="reg">Paused</span>`;
    else if (r?.ok) el.innerHTML = `<span class="reg ok">${ICON.check}Active</span>`;
    else if (r) el.innerHTML = `<span class="reg bad">${ICON.warn}${esc(r.error)}. Choose another shortcut.</span>`;
    else el.innerHTML = "";
  }
}

function startRecording(h, btn) {
  if (S.recording) stopRecording(false);
  S.recording = { h, btn, prev: h.keys };
  btn.classList.add("recording");
  btn.innerHTML = "Press keys…";
  invoke("suspend_hotkeys", { suspend: true });
  btn.focus();
}

function stopRecording(save) {
  const r = S.recording;
  if (!r) return;
  S.recording = null;
  if (!save) r.h.keys = r.prev;
  invoke("suspend_hotkeys", { suspend: false });
  if (save) persist(true);
  if (S.page === "hotkeys") renderHotkeys();
}

const MODS = { Control: "Ctrl", Alt: "Alt", Shift: "Shift", Meta: "Super" };
window.addEventListener("keydown", (e) => {
  if (!S.recording) {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f" && S.page === "history") { e.preventDefault(); $("#h-q")?.focus(); }
    return;
  }
  e.preventDefault();
  e.stopPropagation();
  if (e.key === "Escape") return stopRecording(false);
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean);
  const btn = S.recording.btn;
  if (MODS[e.key]) {
    btn.innerHTML = mods.length ? keycaps(mods.join("+")) + '<span class="plus">&nbsp;+ …</span>' : "Press keys…";
    return;
  }
  let key = e.code;
  if (/^Key[A-Z]$/.test(key)) key = key.slice(3);
  else if (/^Digit\d$/.test(key)) key = key.slice(5);
  const fkey = /^F\d+$/.test(key);
  if (!mods.length && !fkey) {
    btn.innerHTML = '<span class="empty">Add Ctrl, Alt, Shift or Win</span>';
    return;
  }
  S.recording.h.keys = [...mods, key].join("+");
  stopRecording(true);
}, true);
window.addEventListener("blur", () => S.recording && stopRecording(false));

// ------------------------------------------------------------ apps

async function renderApps() {
  const page = $("#page-apps");
  const options = (sel) => S.settings.formats.map((f) => `<option value="${esc(f.id)}" ${sel === f.id ? "selected" : ""}>${esc(f.name)}</option>`).join("");
  const rows = S.settings.profiles.map((p, i) => `
    <div class="row app-row" data-i="${i}">
      ${appIcon(p.app, p.name)}
      <div class="label"><b>${esc(p.name || prettyExe(p.app))}</b><small>${esc(p.app)}</small></div>
      <select class="field" aria-label="Format for ${esc(p.name)}">${options(p.format)}</select>
      <button class="icon-btn" title="Remove">${ICON.x}</button>
    </div>`).join("");
  const known = new Set(S.settings.profiles.map((p) => p.app.toLowerCase()));
  const running = S.apps.filter((a) => !known.has(a.exe.toLowerCase()));
  page.innerHTML = `<div class="scroll"><div class="narrow">
    <h1>Apps</h1>
    <p class="lede">Pick what gets pasted in each program. Word and PowerPoint receive a native, editable equation, and Mathematica gets an expression it can evaluate.</p>
    <div class="group">
      <div class="row"><div class="label"><b>Everywhere else</b><small>Apps that aren’t listed below</small></div>
        <select class="field" id="ap-default" style="width:240px">${options(S.settings.defaultFormat)}</select></div>
    </div>
    <h2>Per app</h2>
    <div class="group" id="ap-group">${rows || '<div class="row"><span class="hint">No apps yet.</span></div>'}</div>
    <div class="add-bar">
      <select class="field" id="ap-running" style="min-width:260px">
        <option value="">Add an open app…</option>
        ${running.map((a) => `<option value="${esc(a.exe)}">${esc(prettyExe(a.exe))}: ${esc(a.title.slice(0, 48))}</option>`).join("")}
      </select>
      <input class="field mono" id="ap-exe" placeholder="or type a program, e.g. Code.exe" spellcheck="false" style="width:240px">
      <button class="btn" id="ap-add">${ICON.plus}Add</button>
    </div>
    <p class="hint">An app is matched by its program file. To find one, open the app, then choose it from “Add an open app”.</p>
  </div></div>`;
  $("#ap-default").addEventListener("change", (e) => { S.settings.defaultFormat = e.target.value; persist(); });
  $$("#ap-group .row[data-i]").forEach((row) => {
    const p = S.settings.profiles[+row.dataset.i];
    row.querySelector("select").addEventListener("change", (e) => { p.format = e.target.value; persist(); });
    row.querySelector(".icon-btn").addEventListener("click", () => { S.settings.profiles.splice(+row.dataset.i, 1); persist(); renderApps(); });
  });
  const add = (exe) => {
    exe = exe.trim();
    if (!exe) return;
    if (S.platform === "windows" && !/\.exe$/i.test(exe)) exe += ".exe";
    if (S.settings.profiles.some((p) => p.app.toLowerCase() === exe.toLowerCase())) return toast(`${exe} is already listed`, true);
    const running = S.apps.find((a) => a.exe.toLowerCase() === exe.toLowerCase());
    S.settings.profiles.push({ app: exe, name: prettyExe(exe), format: S.settings.defaultFormat, path: running?.path || "" });
    persist();
    renderApps();
  };
  $("#ap-running").addEventListener("change", (e) => add(e.target.value));
  $("#ap-add").addEventListener("click", () => add($("#ap-exe").value));
  $("#ap-exe").addEventListener("keydown", (e) => e.key === "Enter" && add(e.target.value));
  if (!S.apps.length) {
    S.apps = await invoke("running_apps");
    if (S.apps.length && S.page === "apps") return renderApps();
  }
  const pathOf = (exe) => S.apps.find((a) => a.exe.toLowerCase() === exe.toLowerCase())?.path || "";
  if (await loadIcons(S.settings.profiles.map((p) => [p.app, p.path || pathOf(p.app)])) && S.page === "apps") renderApps();
}

// ------------------------------------------------------------ formats

function renderFormats() {
  const page = $("#page-formats");
  const rows = S.settings.formats.map((f, i) => `
    <div class="row stack" data-i="${i}">
      <div class="fmt-row">
        <input class="field" data-k="name" value="${esc(f.name)}" aria-label="Name">
        <input class="field mono" data-k="template" value="${esc(f.template)}" aria-label="Template" spellcheck="false">
        ${f.builtin ? `<span style="width:32px"></span>` : `<button class="icon-btn" data-k="remove" title="Remove">${ICON.x}</button>`}
        <div class="preview">${esc(renderTemplate(f.template, SAMPLE))}</div>
        <div class="keys-row">
          <label>Keys before paste<input class="field mono" data-k="before" value="${esc(f.before || "")}" placeholder="none" spellcheck="false"></label>
          <label>Keys after paste<input class="field mono" data-k="after" value="${esc(f.after || "")}" placeholder="none" spellcheck="false"></label>
          <span class="keys-err" data-err></span>
        </div>
      </div>
    </div>`).join("");
  page.innerHTML = `<div class="scroll"><div class="narrow">
    <h1>Output formats</h1>
    <p class="lede">Each format is a template for the pasted text. <code>{latex}</code> is the recognized LaTeX,
      <code>{latex_str}</code> escapes it for a "double-quoted" string, <code>{latex_sq}</code> for a 'single-quoted' MATLAB string,
      <code>{mathml}</code> becomes an equation in Word, PowerPoint and OneNote, <code>{latex_clean}</code> tidies OCR output for typesetting (Mathematica’s Inline TeX Input), and <code>{latex_wl}</code> rewrites it so Mathematica’s <code>ToExpression</code> evaluates it correctly.
      Keys before/after paste are pressed in the target app, for example <code>Ctrl+4</code> and <code>Enter</code> to typeset in a Mathematica notebook.
      Separate several with commas; <code>wait 300</code> pauses.</p>
    <div class="group" id="fm-group">${rows}</div>
    <div class="add-bar"><button class="btn" id="fm-add">${ICON.plus}Add format</button>
      <button class="btn ghost" id="fm-reset">Restore built-in templates</button></div>
    <p class="hint">Previews use ${esc(SAMPLE)}.</p>
  </div></div>`;
  $$("#fm-group .row").forEach((row) => {
    const f = S.settings.formats[+row.dataset.i];
    row.querySelector('[data-k="name"]').addEventListener("input", (e) => { f.name = e.target.value; persist(); });
    row.querySelector('[data-k="template"]').addEventListener("input", (e) => {
      f.template = e.target.value;
      row.querySelector(".preview").textContent = renderTemplate(f.template, SAMPLE);
      persist();
    });
    for (const k of ["before", "after"]) {
      row.querySelector(`[data-k="${k}"]`).addEventListener("input", async (e) => {
        const v = e.target.value;
        const err = await invoke("validate_keys", { keys: v }).then(() => "", (m) => String(m));
        e.target.classList.toggle("bad", !!err);
        row.querySelector("[data-err]").textContent = err;
        if (!err) { f[k] = v; persist(); }
      });
    }
    row.querySelector('[data-k="remove"]')?.addEventListener("click", () => {
      S.settings.formats.splice(+row.dataset.i, 1);
      persist();
      renderFormats();
    });
  });
  $("#fm-add").addEventListener("click", () => {
    S.settings.formats.push({ id: "custom" + Date.now(), name: "New format", template: "{latex}", builtin: false, before: "", after: "" });
    persist();
    renderFormats();
    $$("#fm-group .row").at(-1).querySelector("input").select();
  });
  $("#fm-reset").addEventListener("click", () => {
    S.settings.formats = S.settings.formats.filter((f) => !f.builtin); // the backend re-adds the defaults
    persist(true);
    setTimeout(refreshSettings, 400);
  });
}

// ------------------------------------------------------------ engine

function renderEngine() {
  const page = $("#page-engine");
  const sv = S.settings.server;
  page.innerHTML = `<div class="scroll"><div class="narrow">
    <h1>OCR engine</h1>
    <p class="lede">Recognition runs on this computer with the Nougat LaTeX model. Integrand starts the engine by itself and restarts it if it stops. Nothing is sent over the internet.</p>
    <div class="group"><div class="engine-card" id="en-card"></div></div>
    <div class="add-bar"><button class="btn" id="en-setup">Set up or repair…</button><button class="btn" id="en-restart">${ICON.rerun}Restart engine</button>
      <button class="btn ghost" id="en-logs">Open log folder</button></div>
    <h2>Advanced</h2>
    <div class="group">
      <div class="row stack">
        <div class="label"><b>Python interpreter</b><small>Must have <code>latex_ocr_server</code> installed</small></div>
        <div class="path-row">
          <input class="field mono" id="en-python" value="${esc(sv.python)}" spellcheck="false">
          <button class="btn" id="en-browse-py">Browse…</button>
          <button class="btn" id="en-detect">Find installs</button>
        </div>
        <div class="candidates" id="en-cands"></div>
      </div>
      <div class="row stack">
        <div class="label"><b>Model folder</b><small>Where the model (~1.3 GB) is kept. It is downloaded here on first start if missing.</small></div>
        <div class="path-row"><input class="field mono" id="en-cache" value="${esc(sv.cacheDir)}" spellcheck="false" placeholder="Default folder">
          <button class="btn" id="en-browse-cache">Browse…</button></div>
      </div>
      <div class="row"><div class="label"><b>Port</b><small>Local port the engine listens on</small></div>
        <input class="field" id="en-port" type="number" min="1024" max="65535" value="${sv.port}" style="width:110px"></div>
      <div class="row"><div class="label"><b>Start the engine automatically</b><small>At launch, and again if it stops</small></div>
        <label class="switch"><input type="checkbox" id="en-auto" ${sv.autostart ? "checked" : ""}><i></i></label></div>
      <div class="row"><div class="label"><b>Use CPU only</b><small>Much slower; for computers without an NVIDIA GPU</small></div>
        <label class="switch"><input type="checkbox" id="en-cpu" ${sv.cpu ? "checked" : ""}><i></i></label></div>
    </div>
  </div></div>`;
  renderEngineCard();
  renderCandidates();
  const bind = (id, key, get = (el) => el.value) => $(id).addEventListener("change", (e) => { sv[key] = get(e.target); persist(true); });
  bind("#en-python", "python");
  bind("#en-cache", "cacheDir");
  bind("#en-port", "port", (el) => Math.max(1024, Math.min(65535, parseInt(el.value, 10) || 50051)));
  bind("#en-auto", "autostart", (el) => el.checked);
  bind("#en-cpu", "cpu", (el) => el.checked);
  $("#en-restart").addEventListener("click", async (e) => {
    e.currentTarget.disabled = true;
    try { await invoke("restart_engine"); toast("Engine restarted"); } catch (err) { toast(String(err), true); }
    finally { $("#en-restart") && ($("#en-restart").disabled = false); }
  });
  $("#en-logs").addEventListener("click", () => invoke("open_folder", { which: "data" }));
  $("#en-setup").addEventListener("click", () => {
    if (SETUP.phase !== "running") SETUP.phase = "choose";
    SETUP.info = null;
    SETUP.checks = null;
    showSetup();
  });
  $("#en-browse-py").addEventListener("click", async () => {
    const p = await T.dialog.open({ title: "Choose Python", filters: S.platform === "windows" ? [{ name: "Python", extensions: ["exe"] }] : [] });
    if (p) { sv.python = p; $("#en-python").value = p; persist(true); }
  });
  $("#en-browse-cache").addEventListener("click", async () => {
    const p = await T.dialog.open({ title: "Choose model folder", directory: true });
    if (p) { sv.cacheDir = p; $("#en-cache").value = p; persist(true); }
  });
  $("#en-detect").addEventListener("click", async (e) => {
    const b = e.currentTarget;
    b.disabled = true; b.textContent = "Searching…";
    S.candidates = await invoke("detect_python");
    b.disabled = false; b.textContent = "Find installs";
    renderCandidates();
  });
}

function renderCandidates() {
  const box = $("#en-cands");
  if (!box || !S.candidates) return;
  if (!S.candidates.length) { box.innerHTML = '<span class="hint">No Python found on PATH. Use Browse to pick one.</span>'; return; }
  box.innerHTML = S.candidates.map((c) => `
    <button class="cand ${c.hasServer ? "" : "no"}" data-p="${esc(c.path)}" ${c.hasServer ? "" : 'title="latex_ocr_server is not installed here"'}>
      <span class="reg ${c.hasServer ? "ok" : "bad"}">${c.hasServer ? ICON.check : ICON.warn}</span>
      <code>${esc(c.path)}</code><small>Python ${esc(c.version)}${c.hasServer ? "" : ", no server"}</small></button>`).join("");
  $$(".cand", box).forEach((b) => b.addEventListener("click", () => {
    S.settings.server.python = b.dataset.p;
    $("#en-python").value = b.dataset.p;
    persist(true);
    toast("Python updated");
  }));
}

function renderEngineCard() {
  const card = $("#en-card");
  if (!card || !S.status) return;
  const s = S.status;
  const title = { ready: "Ready", busy: "Recognizing", snipping: "Waiting for your selection", loading: "Starting", offline: "Not running", error: "Needs attention", setup: "Not set up yet" }[s.state] || s.state;
  card.innerHTML = `<i class="dot ${s.state}"></i>
    <div><div class="big">${esc(title)}</div><small class="hint">${esc(s.message)}</small></div>
    <div class="stats">
      <div><small>Device</small><b>${esc(s.device ? s.device.toUpperCase() : "Unknown")}</b></div>
      <div><small>Port</small><b>${s.port}</b></div>
      <div><small>Last conversion</small><b>${s.lastMs != null ? s.lastMs + " ms" : "None yet"}</b></div>
    </div>`;
}

// ------------------------------------------------------------ general

function renderGeneral() {
  const st = S.settings;
  const page = $("#page-general");
  const toggle = (id, on, title, sub) => `<div class="row"><div class="label"><b>${title}</b><small>${sub}</small></div>
    <label class="switch"><input type="checkbox" id="${id}" ${on ? "checked" : ""}><i></i></label></div>`;
  page.innerHTML = `<div class="scroll"><div class="narrow">
    <h1>General</h1>
    <p class="lede">Integrand lives in the system tray. Closing this window keeps it running.</p>
    <div class="group">
      ${toggle("g-login", st.startAtLogin, "Start at login", "Hotkeys are ready as soon as you sign in")}
      ${toggle("g-hud", st.showHud, "Show a confirmation after each conversion", "A small preview in the corner of the screen")}
      ${toggle("g-restore", st.restoreClipboard, "Put the image back on the clipboard after pasting", "Otherwise the pasted text stays on the clipboard")}
      ${toggle("g-pause", S.paused, "Pause all hotkeys", "Also available from the tray menu")}
      <div class="row"><div class="label"><b>Appearance</b><small>Follows Windows by default</small></div>
        <select class="field" id="g-theme" style="width:160px">
          ${["system", "light", "dark"].map((t) => `<option value="${t}" ${st.theme === t ? "selected" : ""}>${{ system: "Match system", light: "Light", dark: "Dark" }[t]}</option>`).join("")}
        </select></div>
    </div>
    <h2>History</h2>
    <div class="group">
      ${toggle("g-images", st.saveImages, "Keep source images", "Needed to compare and run recognition again")}
      <div class="row"><div class="label"><b>Keep the last</b><small>Starred items are never removed</small></div>
        <select class="field" id="g-limit" style="width:160px">
          ${[100, 250, 500, 1000, 5000].map((n) => `<option value="${n}" ${st.historyLimit === n ? "selected" : ""}>${n} conversions</option>`).join("")}
        </select></div>
      <div class="row"><div class="label"><b>Clear history</b><small>Starred items are kept</small></div>
        <button class="btn danger" id="g-clear">Clear history</button></div>
      <div class="row"><div class="label"><b>Data folder</b><small class="selectable">${esc(S.dataDir)}</small></div>
        <button class="btn" id="g-open">Open</button></div>
    </div>
  </div></div>`;
  const bind = (id, key) => $(id).addEventListener("change", (e) => { st[key] = e.target.checked; persist(true); });
  bind("#g-login", "startAtLogin");
  bind("#g-hud", "showHud");
  bind("#g-restore", "restoreClipboard");
  bind("#g-images", "saveImages");
  $("#g-pause").addEventListener("change", async (e) => { S.paused = e.target.checked; await invoke("set_paused", { paused: S.paused }); renderStatus(); });
  $("#g-theme").addEventListener("change", (e) => { st.theme = e.target.value; applyTheme(); persist(true); });
  $("#g-limit").addEventListener("change", (e) => { st.historyLimit = +e.target.value; persist(true); });
  $("#g-open").addEventListener("click", () => invoke("open_folder", { which: "data" }));
  $("#g-clear").addEventListener("click", async (e) => {
    const b = e.currentTarget;
    if (b.dataset.confirm !== "1") { b.dataset.confirm = "1"; b.textContent = "Click again to clear"; setTimeout(() => { b.dataset.confirm = ""; b.textContent = "Clear history"; }, 3000); return; }
    await invoke("history_clear", { keepFavorites: true });
    S.history = S.history.filter((x) => x.favorite);
    b.dataset.confirm = ""; b.textContent = "Clear history";
    toast("History cleared");
  });
}

// ------------------------------------------------------------ about

function renderAbout() {
  const k = (a) => keycaps(hotkeyFor(a));
  $("#page-about").innerHTML = `<div class="scroll"><div class="narrow">
    <div class="about"><img src="logo.svg" alt="">
      <div><h1>Integrand</h1><p class="lede" style="margin:4px 0 0">Version ${esc(S.version)}. Turns a picture of math into LaTeX in any app.</p></div></div>
    <h2>Shortcuts</h2>
    <div class="group">
      <div class="row"><div class="label"><b>Snip and paste</b><small>Select part of the screen; the result goes to your cursor</small></div>${k("snipPaste")}</div>
      <div class="row"><div class="label"><b>Paste clipboard image</b><small>For an image you’ve already copied</small></div>${k("clipboardPaste")}</div>
      <div class="row"><div class="label"><b>Open history</b><small>Everything you’ve converted</small></div>${k("openHistory")}</div>
    </div>
    <h2>Built with</h2>
    <div class="group">
      <div class="row"><div class="label"><b>Nougat LaTeX</b><small>Recognition model (Norm/nougat-latex-base)</small></div></div>
      <div class="row"><div class="label"><b>latex_ocr_server</b><small>Recognition server by Lucas Van Mol (MIT)</small></div></div>
      <div class="row"><div class="label"><b>KaTeX, Tauri, pulldown-latex</b><small>Equation rendering, app shell, LaTeX to MathML</small></div></div>
    </div>
  </div></div>`;
}

const RENDER = { history: renderHistory, hotkeys: renderHotkeys, apps: renderApps, formats: renderFormats, engine: renderEngine, general: renderGeneral, about: renderAbout };

// ------------------------------------------------------------ engine setup
//
// Three phases, like a regular installer:
//   before:   check this computer and show what will be downloaded (engine_info, engine_preflight)
//   install:  uv, Python, PyTorch, server, model
//   finish:   clean up, test, switch over; then a summary with the start-at-login option

const PHASES = ["Before installing", "Installing", "Finishing"];
const STEPS = {
  install: [
    ["uv", "Python manager", "Downloads uv, a small tool that installs Python privately"],
    ["python", "Python 3.13", "A private copy just for Integrand"],
    ["torch", "PyTorch", "The library the model runs on"],
    ["server", "Recognition server", "latex_ocr_server and its libraries"],
    ["model", "Recognition model", "Nougat LaTeX, about 1.4 GB"],
  ],
  finish: [
    ["cleanup", "Clean up", "Removes the download cache and files only needed for building"],
    ["verify", "Test", "Recognizes a sample equation"],
    ["configure", "Switch over", "Points Integrand at the new engine"],
  ],
};
const SETUP = { el: null, info: null, checks: null, mode: "auto", own: null, steps: {}, phase: "choose", error: null, latex: null, disk: null, log: [] };

async function showSetup() {
  if (SETUP.el) return;
  SETUP.el = document.createElement("div");
  SETUP.el.className = "welcome";
  document.body.append(SETUP.el);
  renderSetup();
  if (!SETUP.info) {
    SETUP.info = await invoke("engine_info");
    renderSetup();
  }
  if (SETUP.phase === "choose" && !SETUP.checks) runChecks();
}

async function runChecks() {
  SETUP.checks = null;
  renderSetup();
  try {
    SETUP.checks = await invoke("engine_preflight");
  } catch (e) {
    SETUP.checks = [{ id: "err", label: "Checks", state: "fail", detail: String(e) }];
  }
  renderSetup();
}

function closeSetup() {
  SETUP.el?.remove();
  SETUP.el = null;
  if (S.page === "engine") renderEngine();
}

const size = (b) => (b >= 1e9 ? `${(b / 1e9).toFixed(1)} GB` : `${Math.round(b / 1e6)} MB`);

function setupPhases() {
  const current = SETUP.phase === "choose" ? 0
    : SETUP.phase === "done" ? 3
    : STEPS.finish.some(([id]) => SETUP.steps[id]) ? 2 : 1;
  return `<ol class="phases">${PHASES.map((title, n) =>
    `<li class="${n < current ? "done" : n === current ? "on" : ""}"><span>${n < current ? ICON.check : n + 1}</span>${title}</li>`).join("")}</ol>`;
}

function setupHardware(i) {
  if (!i) return '<p class="hint">Checking this computer…</p>';
  return `<div class="sysline">
      <div><small>Graphics</small><b>${esc(i.gpu || "No dedicated GPU")}</b>${i.driver ? `<small>Driver ${esc(i.driver)}</small>` : ""}</div>
      <div><small>Runs on</small><b>${esc(i.accel)}</b></div>
      <div><small>Download</small><b>${i.downloadGb ? `About ${i.downloadGb} GB` : "Nothing new"}</b><small>${i.diskGb} GB on disk</small></div>
    </div>`;
}

function setupChecks() {
  if (!SETUP.checks) return '<ul class="checks"><li><span class="si"><span class="spin"></span></span><div><b>Checking this computer</b><small>Graphics, disk space and the download servers</small></div></li></ul>';
  return `<ul class="checks">${SETUP.checks.map((c) =>
    `<li class="${c.state}"><span class="si">${c.state === "ok" ? ICON.check : ICON.warn}</span><div><b>${esc(c.label)}</b><small>${esc(c.detail)}</small></div></li>`).join("")}</ul>`;
}

function setupDownloads(i) {
  if (!i) return "";
  const rows = i.items.map((it) =>
    `<tr class="${it.present ? "present" : ""}"><td><b>${esc(it.name)}</b><small>${esc(it.purpose)}</small></td>
      <td class="num">${it.present ? "Already here" : size(it.bytes)}</td></tr>`).join("");
  return `<details class="downloads"><summary>What gets downloaded, and why</summary>
    <table>${rows}<tr class="total"><td>Total download</td><td class="num">${i.downloadGb ? `${i.downloadGb} GB` : "Nothing"}</td></tr></table>
    <p class="hint">All of it is required. Everything goes into Integrand’s own data folder, and “Delete app data” in the uninstaller removes it.</p></details>`;
}

function setupChoose(i) {
  const own = SETUP.own;
  const ownList = own == null
    ? '<span class="hint">Looking for Python installs…</span>'
    : own.filter((c) => c.hasServer).map((c) =>
        `<button class="cand" data-p="${esc(c.path)}"><span class="reg ok">${ICON.check}</span><code>${esc(c.path)}</code><small>Python ${esc(c.version)}</small></button>`).join("")
      || '<span class="hint">No Python with latex_ocr_server was found. Use the automatic install.</span>';
  const blocked = SETUP.checks?.some((c) => c.state === "fail");
  const body = `${setupHardware(i)}${setupChecks()}${setupDownloads(i)}
    <div class="choices" role="radiogroup" aria-label="Setup method">
      <label class="choice ${SETUP.mode === "auto" ? "on" : ""}"><input type="radio" name="mode" value="auto" ${SETUP.mode === "auto" ? "checked" : ""}>
        <div><b>Install automatically</b><span class="rec">Recommended</span>
        <p>Python, PyTorch and the model are installed into Integrand’s own folder. You don’t need Python beforehand, nothing else on this computer changes, and uninstalling removes it all.</p></div></label>
      <label class="choice ${SETUP.mode === "own" ? "on" : ""}"><input type="radio" name="mode" value="own" ${SETUP.mode === "own" ? "checked" : ""}>
        <div><b>Use a Python I already have</b>
        <p>For a Python that already has <code>latex_ocr_server</code> and PyTorch installed.</p>
        ${SETUP.mode === "own" ? `<div class="candidates">${ownList}</div>` : ""}</div></label>
    </div>`;
  const ready = i && SETUP.checks && !blocked && SETUP.mode === "auto";
  const foot = `${blocked ? '<button class="btn ghost" id="su-recheck" style="margin-right:auto">Check again</button>' : ""}
    <button class="btn" id="su-later">Later</button>
    <button class="btn primary" id="su-go" ${ready ? "" : "disabled"}>${i?.downloadGb ? `Install (${i.downloadGb} GB)` : "Install"}</button>`;
  return [body, foot];
}

function stepRows(steps, i) {
  return steps.map(([id, title, sub]) => {
    const st = SETUP.steps[id] || {};
    const state = st.state || "pending";
    const icon = { done: ICON.check, skipped: ICON.check, error: ICON.warn, cancelled: ICON.warn, running: '<span class="spin"></span>' }[state] || "";
    const label = id === "torch" && i ? `PyTorch (${i.accel})` : title;
    const msg = state === "running" ? (st.detail || st.message || sub) : state === "skipped" ? "Already done" : state === "pending" ? sub : st.message;
    const bar = state === "running"
      ? `<div class="bar ${st.fraction == null ? "indet" : ""}"><i style="width:${Math.round((st.fraction || 0) * 100)}%"></i></div>` : "";
    return `<li class="${state}"><span class="si">${icon}</span><div><b>${esc(label)}</b><small>${esc(msg || "")}</small>${bar}</div></li>`;
  }).join("");
}

function setupProgress(i) {
  const done = SETUP.phase === "done";
  const body = done
    ? `<div class="setup-done"><span class="reg ok">${ICON.check}The test equation came back as</span><div class="math" id="su-math"></div></div>
       <div class="sysline">
         <div><small>Runs on</small><b>${esc(i?.accel || "")}</b></div>
         <div><small>Disk space used</small><b>${esc(SETUP.disk || `About ${i?.diskGb} GB`)}</b></div>
         <div><small>Snip and paste</small><b>${keycaps(hotkeyFor("snipPaste"))}</b></div>
       </div>
       ${i?.note ? `<p class="hint">${esc(i.note)}</p>` : ""}
       <label class="check-row"><input type="checkbox" id="su-login" ${S.settings.startAtLogin ? "checked" : ""}> Start Integrand when I sign in, so the hotkeys always work</label>`
    : `${setupHardware(i)}
       <h3 class="steps-head">Installation</h3><ol class="steps-list">${stepRows(STEPS.install, i)}</ol>
       <h3 class="steps-head">Post-installation</h3><ol class="steps-list">${stepRows(STEPS.finish, i)}</ol>
       ${SETUP.error ? `<p class="setup-err">${esc(SETUP.error)}</p>` : ""}`;
  const log = `<details class="log"><summary>Details</summary><pre id="su-log">${esc(SETUP.log.slice(-80).join("\n"))}</pre></details>`;
  const foot = SETUP.phase === "running"
    ? `<span class="hint" style="margin-right:auto">You can close this window. Setup continues in the background.</span>
       <button class="btn" id="su-hide">Hide</button><button class="btn danger" id="su-cancel">Cancel</button>`
    : done
      ? `<button class="btn primary" id="su-finish">Finish</button>`
      : `<button class="btn ghost" id="su-logs" style="margin-right:auto">Open log folder</button>
         <button class="btn" id="su-later">Close</button><button class="btn primary" id="su-retry">Retry</button>`;
  return [body + log, foot];
}

function renderSetup() {
  const el = SETUP.el;
  if (!el) return;
  const i = SETUP.info;
  const [body, foot] = SETUP.phase === "choose" ? setupChoose(i) : setupProgress(i);
  const title = { choose: "Set up the recognition engine", done: "Setup is complete" }[SETUP.phase] || "Setting up the recognition engine";
  el.innerHTML = `<div class="sheet setup-sheet" role="dialog" aria-modal="true" aria-labelledby="su-title">
    ${setupPhases()}
    <h1 id="su-title">${title}</h1>
    ${SETUP.phase === "choose" ? '<p class="lede">Integrand reads equations on this computer, so nothing you snip leaves it. This one-time setup downloads what it needs.</p>' : ""}
    ${body}<footer>${foot}</footer></div>`;
  if (SETUP.phase === "done" && SETUP.latex) tex($("#su-math", el), SETUP.latex);
  const on = (id, f) => $(id, el)?.addEventListener("click", f);
  $$('input[name="mode"]', el).forEach((r) => r.addEventListener("change", async () => {
    SETUP.mode = r.value;
    renderSetup();
    if (SETUP.mode === "own" && SETUP.own == null) {
      SETUP.own = await invoke("detect_python");
      renderSetup();
    }
  }));
  $$(".cand", el).forEach((b) => b.addEventListener("click", async () => {
    try {
      await invoke("engine_use_python", { path: b.dataset.p });
      toast("Using " + b.dataset.p);
      closeSetup();
    } catch (e) {
      toast(String(e), true);
    }
  }));
  const start = () => {
    SETUP.phase = "running";
    SETUP.error = null;
    SETUP.steps = {};
    renderSetup();
    invoke("engine_install");
  };
  on("#su-go", start);
  on("#su-retry", start);
  on("#su-recheck", runChecks);
  on("#su-later", closeSetup);
  on("#su-hide", closeSetup);
  on("#su-finish", () => {
    const login = $("#su-login", el)?.checked;
    if (login != null && login !== S.settings.startAtLogin) {
      S.settings.startAtLogin = login;
      persist(true);
    }
    closeSetup();
    go("history");
  });
  on("#su-cancel", () => invoke("engine_cancel"));
  on("#su-logs", () => invoke("open_folder", { which: "data" }));
  const log = $("#su-log", el);
  if (log) log.scrollTop = log.scrollHeight;
}

function onSetupEvent(p) {
  if (p.detail && p.state === "running" && !/ of about /.test(p.detail) && SETUP.log.at(-1) !== p.detail) SETUP.log.push(p.detail);
  if (p.message && p.state !== "running") SETUP.log.push(`${p.step}: ${p.message}`);
  if (p.step === "done") {
    SETUP.phase = "done";
    SETUP.latex = p.message;
    SETUP.disk = p.detail;
  } else {
    const prev = SETUP.steps[p.step] || {};
    SETUP.steps[p.step] = { ...prev, ...p, message: p.message || prev.message };
    if (p.state === "error" || p.state === "cancelled") {
      SETUP.phase = "failed";
      SETUP.error = p.state === "cancelled" ? "Setup was cancelled. Retry continues where it stopped." : p.message;
    } else {
      SETUP.phase = "running";
    }
  }
  if (!SETUP.el && (p.step === "done" || p.state === "error")) showSetup();
  else renderSetup();
}

// ------------------------------------------------------------ welcome

function showWelcome() {
  const el = document.createElement("div");
  el.className = "welcome";
  el.innerHTML = `<div class="sheet" role="dialog" aria-modal="true" aria-labelledby="w-title">
    <img src="logo.svg" alt="" width="56">
    <h1 id="w-title">Welcome to Integrand</h1>
    <p class="lede" style="margin:0">Turn any equation on your screen into LaTeX, right where you’re typing.</p>
    <div class="steps">
      <div class="step"><span class="n">1</span><div><b>Click where the math should go</b><span>Word, PowerPoint, Mathematica, MATLAB, a notebook, anything.</span></div></div>
      <div class="step"><span class="n">2</span><div><b>Press ${keycaps(hotkeyFor("snipPaste"))} and drag over an equation</b><span>A PDF, a slide, a photo, a web page.</span></div></div>
      <div class="step"><span class="n">3</span><div><b>It’s pasted, formatted for that app</b><span>Native equations in Word and PowerPoint, expressions in Mathematica.</span></div></div>
    </div>
    <footer><button class="btn" id="w-keys">Change hotkeys</button><button class="btn primary" id="w-go">Get started</button></footer>
  </div>`;
  document.body.append(el);
  const done = (page) => {
    S.settings.onboarded = true;
    persist(true);
    el.remove();
    go(page);
    if (S.status?.state === "setup") showSetup();
  };
  $("#w-go").addEventListener("click", () => done("history"));
  $("#w-keys").addEventListener("click", () => done("hotkeys"));
  $("#w-go").focus();
}

// ------------------------------------------------------------ boot

async function refreshSettings() {
  const st = await invoke("get_state");
  S.settings = st.settings;
  S.results = st.hotkeyResults;
  if (RENDER[S.page]) RENDER[S.page]();
}

async function boot() {
  // Subscribe before reading state, so an event fired while the window loads isn't missed.
  const { listen } = T.event;
  await Promise.all([
    listen("status", (e) => {
      const was = S.status?.state;
      S.status = e.payload;
      if (!S.settings) return; // still booting; boot() renders it
      renderStatus();
      if (S.status.state === "setup" && was !== "setup" && S.settings.onboarded && !SETUP.el && SETUP.phase === "choose") showSetup();
    }),
    listen("setup", (e) => onSetupEvent(e.payload)),
    listen("paused", (e) => { S.paused = e.payload; if (S.settings) renderStatus(); if (S.page === "hotkeys") updateHotkeyStatus(); }),
    listen("navigate", (e) => S.settings && go(e.payload)),
    listen("history-added", (e) => {
      S.history.unshift(e.payload);
      S.sel = e.payload.id;
      if (S.page === "history" && S.settings) renderList();
    }),
  ]);

  const st = await invoke("get_state");
  Object.assign(S, { settings: st.settings, results: st.hotkeyResults, paused: st.paused, version: st.version, dataDir: st.dataDir, platform: st.platform });
  S.status = S.status || st.status;
  S.history = await invoke("history_list");
  applyTheme();
  $$(".nav button, #engine-pill").forEach((b) => b.addEventListener("click", () => go(b.dataset.page)));
  renderStatus();
  go("history");
  if (!S.settings.onboarded) showWelcome();
  else if (S.status?.state === "setup") showSetup();

  const win = T.webviewWindow.getCurrentWebviewWindow();
  win.onFocusChanged(({ payload: focused }) => { if (focused) S.apps = []; }); // refresh the open-apps list next time
  win.onDragDropEvent(async (ev) => {
    const { type, paths } = ev.payload;
    document.body.classList.toggle("dropping", type === "over" || type === "enter");
    if (type !== "drop") return;
    document.body.classList.remove("dropping");
    const images = (paths || []).filter((p) => /\.(png|jpe?g|bmp|gif|webp|tiff?)$/i.test(p));
    if (!images.length) return toast("Drop a PNG, JPG or other image file", true);
    go("history");
    for (const p of images) {
      try {
        const e = await invoke("ocr_file", { path: p });
        toast(`Converted and copied (${e.ms} ms)`);
      } catch (err) {
        toast(String(err), true);
      }
    }
  });
}

window.addEventListener("resize", () => {
  const e = S.page === "history" && S.history.find((x) => x.id === S.sel);
  if (e && $("#d-math")) texFit($("#d-math"), e.latex);
});

boot().catch((e) => {
  document.body.innerHTML = `<pre style="padding:24px;color:#c2410c">Integrand failed to start the window:\n${esc(e?.stack || e)}</pre>`;
});
