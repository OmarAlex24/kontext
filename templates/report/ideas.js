// kontext report, "Your ideas": show only one state, and a cell of the proof
// strip brings back every idea before it jumps to its row. Without this
// script every group is in view.
(() => {
  "use strict";
  const sec = document.getElementById("your-ideas");
  if (!sec) return;
  const btns = [...sec.querySelectorAll(".st-btn")];
  const show = (state) => {
    for (const b of btns) b.setAttribute("aria-pressed", String(b.dataset.state === state));
    for (const g of sec.querySelectorAll(".idea-group")) g.hidden = !!state && g.dataset.state !== state;
  };
  for (const b of btns) b.addEventListener("click", () => show(b.dataset.state));
  for (const c of sec.querySelectorAll(".cell")) c.addEventListener("click", () => show(""));
})();
