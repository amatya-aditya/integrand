"use strict";
// Corner confirmation shown after each conversion; the backend positions, shows and hides the window.
const card = document.getElementById("card");
const ICONS = {
  ok: '<svg viewBox="0 0 24 24"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>',
  error: '<svg viewBox="0 0 24 24"><path d="M12 7.5v5.5M12 16.5h.01"/></svg>',
  working: '<div class="spin"></div>',
};

window.__TAURI__.event.listen("hud", ({ payload: h }) => {
  card.className = "card " + h.kind;
  document.getElementById("badge").innerHTML = ICONS[h.kind] || "";
  document.getElementById("title").textContent = h.title;
  document.getElementById("detail").textContent = h.detail || "";
  const eq = document.getElementById("eq");
  eq.style.display = h.latex ? "" : "none";
  if (h.latex) {
    try {
      eq.style.fontSize = "";
      katex.render(h.latex, eq, { displayMode: true, throwOnError: false, strict: "ignore" });
      // Shrink long or tall equations to fit the card.
      const k = eq.querySelector(".katex");
      let size = 16;
      eq.classList.remove("fade");
      while (k && size > 9 && (eq.scrollWidth > eq.clientWidth + 1 || k.offsetHeight > 66)) eq.style.fontSize = --size + "px";
      eq.classList.toggle("fade", eq.scrollWidth > eq.clientWidth + 1);
    } catch {
      eq.textContent = h.latex;
    }
  }
  requestAnimationFrame(() => card.classList.add("show"));
});
