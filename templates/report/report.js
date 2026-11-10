// kontext report: the little that moves. Copies commands, compares the before
// and after, enlarges screenshots and dithers them into a <canvas>. No network,
// no dependencies and no writes: without this script the page reads the same.
(() => {
  "use strict";
  const doc = document;
  const root = doc.documentElement;
  root.classList.add("js");

  const live = doc.createElement("p");
  live.className = "sr";
  live.setAttribute("aria-live", "polite");
  doc.body.append(live);

  // ------------------------------------------------ copiar
  const copyText = async (text) => {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (_) {
      const ta = doc.createElement("textarea");
      ta.value = text;
      ta.setAttribute("readonly", "");
      ta.className = "sr";
      doc.body.append(ta);
      ta.select();
      let ok = false;
      try {
        ok = doc.execCommand("copy");
      } catch (_) {
        ok = false;
      }
      ta.remove();
      return ok;
    }
  };
  // The main command has "Copy"; the discreet ones (`.mini`), an icon.
  for (const box of doc.querySelectorAll(".cmdbox")) {
    const code = box.querySelector("code");
    const text = code.textContent;
    const icon = box.classList.contains("mini");
    const b = doc.createElement("button");
    b.type = "button";
    b.className = icon ? "copy icon" : "copy";
    if (!icon) b.textContent = "Copy";
    b.setAttribute("aria-label", "Copy " + text);
    if (icon) b.title = "Copy";
    let timer = 0;
    b.addEventListener("click", async () => {
      const ok = await copyText(text);
      if (!ok) {
        const range = doc.createRange();
        range.selectNodeContents(code);
        const sel = getSelection();
        sel.removeAllRanges();
        sel.addRange(range);
      }
      if (!icon) b.textContent = ok ? "Copied" : "Select it";
      if (icon) b.title = ok ? "Copied" : "Select it";
      b.classList.toggle("done", ok);
      live.textContent = ok ? "Copied: " + text : "Could not copy; it was selected instead";
      clearTimeout(timer);
      timer = setTimeout(() => {
        if (!icon) b.textContent = "Copy";
        if (icon) b.title = "Copy";
        b.classList.remove("done");
      }, 1600);
    });
    box.append(b);
  }

  // ------------------------------------------------ trama (Bayer 8×8, 2 tintas)
  // The same dither family as the CSS, but finer (one dot per CSS pixel) so
  // the screen can be recognized.
  const DOT = 1;
  const BAYER8 = [
    0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26,
    12, 44, 4, 36, 14, 46, 6, 38, 60, 28, 52, 20, 62, 30, 54, 22,
    3, 35, 11, 43, 1, 33, 9, 41, 51, 19, 59, 27, 49, 17, 57, 25,
    15, 47, 7, 39, 13, 45, 5, 37, 63, 31, 55, 23, 61, 29, 53, 21,
  ];
  const probe = doc.createElement("canvas").getContext("2d", { willReadFrequently: true });
  const rgb = (css) => {
    probe.clearRect(0, 0, 1, 1);
    probe.fillStyle = "#000";
    probe.fillStyle = css.trim();
    probe.fillRect(0, 0, 1, 1);
    return probe.getImageData(0, 0, 1, 1).data;
  };
  const tones = () => {
    const s = getComputedStyle(root);
    return [rgb(s.getPropertyValue("--dit-dark")), rgb(s.getPropertyValue("--dit-light"))];
  };
  let ink = tones();

  const dither = (img, canvas) => {
    const w = canvas.clientWidth;
    const h = canvas.clientHeight;
    if (!w || !h || !img.naturalWidth) return;
    const cw = Math.max(1, Math.round(w / DOT));
    const ch = Math.max(1, Math.round(h / DOT));
    canvas.width = cw;
    canvas.height = ch;
    const g = canvas.getContext("2d", { willReadFrequently: true });
    const [dark, light] = ink;
    g.fillStyle = "#fff";
    g.fillRect(0, 0, cw, ch);
    const s = Math.min(cw / img.naturalWidth, ch / img.naturalHeight);
    const dw = img.naturalWidth * s;
    const dh = img.naturalHeight * s;
    g.imageSmoothingQuality = "high";
    g.drawImage(img, (cw - dw) / 2, (ch - dh) / 2, dw, dh);
    let data;
    try {
      data = g.getImageData(0, 0, cw, ch);
    } catch (_) {
      // An image from another origin (a file opened by hand): it goes in color.
      canvas.remove();
      return;
    }
    const px = data.data;
    for (let y = 0, i = 0; y < ch; y++) {
      for (let x = 0; x < cw; x++, i += 4) {
        const l = (0.2126 * px[i] + 0.7152 * px[i + 1] + 0.0722 * px[i + 2]) / 255;
        // A bit darker (γ 1.5) so the light grays do not get lost.
        const on = 64 * Math.pow(l, 1.5) > BAYER8[((y & 7) << 3) | (x & 7)] + 0.5;
        const c = on ? light : dark;
        px[i] = c[0];
        px[i + 1] = c[1];
        px[i + 2] = c[2];
        px[i + 3] = 255;
      }
    }
    g.putImageData(data, 0, 0);
  };

  const pairs = [];
  const addCanvas = (holder, img) => {
    const c = doc.createElement("canvas");
    c.setAttribute("aria-hidden", "true");
    holder.append(c);
    const pair = { img, c };
    pairs.push(pair);
    const draw = () => dither(img, c);
    if (img.complete && img.naturalWidth) draw();
    else img.addEventListener("load", draw, { once: true });
    return pair;
  };
  let pending = 0;
  const redrawAll = () => {
    cancelAnimationFrame(pending);
    pending = requestAnimationFrame(() => {
      ink = tones();
      for (const p of pairs) if (p.c.isConnected) dither(p.img, p.c);
    });
  };
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", redrawAll);
  let lastW = innerWidth;
  addEventListener("resize", () => {
    if (innerWidth !== lastW) {
      lastW = innerWidth;
      redrawAll();
    }
  });

  // ------------------------------------------------ comparador
  const el = (tag, cls, text) => {
    const n = doc.createElement(tag);
    if (cls) n.className = cls;
    if (text) n.textContent = text;
    return n;
  };
  const buildCompare = (before, now, label, withCanvas, start) => {
    const cmp = el("div", "frame compare");
    const lb = el("div", "layer before");
    const ln = el("div", "layer now");
    lb.append(before);
    ln.append(now);
    cmp.append(lb, ln);
    if (withCanvas) {
      addCanvas(lb, before);
      addCanvas(ln, now);
    }
    const handle = el("div", "handle");
    handle.append(el("span", "knob"));
    const r = el("input", "slider");
    r.type = "range";
    r.min = "0";
    r.max = "100";
    r.step = "1";
    r.value = String(start);
    r.setAttribute("aria-label", "Compare the before and now of " + label);
    r.setAttribute("aria-valuetext", "");
    const set = () => {
      cmp.style.setProperty("--pos", r.value + "%");
      r.setAttribute("aria-valuetext", r.value + " % of the before in view");
    };
    r.addEventListener("input", set);
    set();
    cmp.append(handle, el("span", "tag tag-before", "Before"), el("span", "tag tag-now", "Now"), r);
    return { cmp, r };
  };

  // ------------------------------------------------ vista ampliada
  const dlg = el("dialog", "zoom");
  const head = el("div", "zoom-head");
  const title = el("p", "zoom-title");
  title.id = "zoom-title";
  const close = el("button", "zoom-close", "Close");
  close.type = "button";
  head.append(title, close);
  const body = el("div", "zoom-body");
  const cap = el("p", "zoom-cap");
  dlg.append(head, body, cap);
  dlg.setAttribute("aria-labelledby", "zoom-title");
  doc.body.append(dlg);
  let opener = null;
  close.addEventListener("click", () => dlg.close());
  dlg.addEventListener("click", (e) => {
    if (e.target === dlg) dlg.close();
  });
  dlg.addEventListener("close", () => {
    body.replaceChildren();
    if (opener) opener.focus();
  });
  const fill = (shot) => {
    title.replaceChildren();
    for (const s of shot.querySelectorAll("h3 > span")) title.append(el("span", s.className, s.textContent));
  };
  const openSingle = (shot, img, caption, from) => {
    if (typeof dlg.showModal !== "function") return;
    fill(shot);
    const big = el("img");
    big.src = img.currentSrc || img.src;
    big.alt = img.alt;
    big.style.setProperty("--nw", (img.naturalWidth || 600) + "px");
    body.replaceChildren(big);
    cap.textContent = caption;
    opener = from;
    dlg.showModal();
  };
  const openCompare = (shot, before, now, value, from) => {
    if (typeof dlg.showModal !== "function") return;
    fill(shot);
    const b = before.cloneNode();
    const n = now.cloneNode();
    b.loading = n.loading = "eager";
    const { cmp, r } = buildCompare(b, n, shot.querySelector(".screen").textContent, false, value);
    const w = now.naturalWidth || 4;
    const h = now.naturalHeight || 3;
    cmp.style.setProperty("--ar", w + " / " + h);
    cmp.style.setProperty("--arn", String(w / h));
    cmp.style.setProperty("--nw", (now.naturalWidth || 600) + "px");
    body.replaceChildren(cmp);
    cap.textContent = [...shot.querySelectorAll("figcaption")].map((f) => f.textContent.trim()).join("  ·  ");
    opener = from;
    dlg.showModal();
    r.focus();
  };

  // ------------------------------------------------ the screenshots
  for (const shot of doc.querySelectorAll(".shot")) {
    const imgs = [...shot.querySelectorAll(".pair img")];
    const label = shot.querySelector(".screen").textContent;
    if (shot.classList.contains("has-before") && imgs.length === 2) {
      const keep = imgs.map((i) => i.cloneNode());
      for (const k of keep) k.loading = "eager";
      const { cmp, r } = buildCompare(keep[0], keep[1], label, true, 50);
      const zb = el("button", "zoom-btn", "Zoom");
      zb.type = "button";
      zb.setAttribute("aria-label", "Zoom " + label);
      zb.addEventListener("click", () => openCompare(shot, keep[0], keep[1], r.value, zb));
      cmp.append(zb);
      shot.classList.add("cmp");
      shot.querySelector(".pair").before(cmp);
      continue;
    }
    for (const fig of shot.querySelectorAll("figure")) {
      const frame = fig.querySelector(".frame");
      const img = frame && frame.querySelector("img");
      if (!img) continue;
      addCanvas(frame, img);
      frame.classList.add("zoomable");
      frame.tabIndex = 0;
      frame.setAttribute("role", "button");
      frame.setAttribute("aria-label", "Zoom " + img.alt);
      const caption = fig.querySelector("figcaption").textContent.trim();
      const open = () => openSingle(shot, img, caption, frame);
      frame.addEventListener("click", open);
      frame.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          open();
        }
      });
    }
  }

  // ------------------------------------------------ barra lateral
  const side = doc.getElementById("lateral");
  const main = doc.getElementById("contenido");
  const barTop = doc.querySelector(".bar-top");
  const openBtn = doc.querySelector(".bar-btn");
  const small = matchMedia("(max-width: 62.49rem)");
  const sectionIds = ["your-turn", "problems", "your-ideas", "what-it-looks-like", "progress"];
  const sections = sectionIds.map((id) => doc.getElementById(id)).filter(Boolean);
  for (const s of sections) s.tabIndex = -1;

  // The phone panel: it traps focus; Esc or a click outside closes it.
  const scrim = el("div", "scrim");
  doc.body.append(scrim);
  let panelOpen = false;
  const focusables = () =>
    [...side.querySelectorAll("a[href], button:not([disabled])")].filter((n) => n.offsetParent !== null);
  const openPanel = () => {
    panelOpen = true;
    side.classList.add("open");
    side.setAttribute("role", "dialog");
    side.setAttribute("aria-modal", "true");
    scrim.classList.add("on");
    openBtn.setAttribute("aria-expanded", "true");
    for (const n of [main, barTop]) n.inert = true;
    const first = focusables()[0];
    if (first) first.focus();
  };
  const closePanel = (refocus) => {
    if (!panelOpen) return;
    panelOpen = false;
    side.classList.remove("open");
    side.removeAttribute("role");
    side.removeAttribute("aria-modal");
    scrim.classList.remove("on");
    openBtn.setAttribute("aria-expanded", "false");
    for (const n of [main, barTop]) n.inert = false;
    if (refocus) openBtn.focus();
  };
  if (openBtn) openBtn.addEventListener("click", () => (panelOpen ? closePanel(true) : openPanel()));
  const sideX = side.querySelector(".side-x");
  if (sideX) sideX.addEventListener("click", () => closePanel(true));
  scrim.addEventListener("click", () => closePanel(true));
  doc.addEventListener("keydown", (e) => {
    if (!panelOpen) return;
    if (e.key === "Escape") {
      e.preventDefault();
      closePanel(true);
    } else if (e.key === "Tab") {
      const f = focusables();
      if (!f.length) return;
      const first = f[0];
      const last = f[f.length - 1];
      if (e.shiftKey && (doc.activeElement === first || !side.contains(doc.activeElement))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (doc.activeElement === last || !side.contains(doc.activeElement))) {
        e.preventDefault();
        first.focus();
      }
    }
  });
  small.addEventListener("change", () => closePanel(false));

  // Go to a section: in the panel, it closes first; the focus goes to the section.
  const links = [...side.querySelectorAll(".side-nav a[data-nav]")];
  for (const a of links) {
    a.addEventListener("click", (e) => {
      const target = doc.getElementById(a.dataset.nav);
      if (!target) return;
      e.preventDefault();
      closePanel(false);
      pinned = target;
      markActive();
      target.scrollIntoView({ block: "start" });
      target.focus({ preventScroll: true });
      history.replaceState(null, "", "#" + a.dataset.nav);
    });
  }

  // The section in view: the last one whose top edge has passed 35 % of the
  // window (the ones side by side count as the first), or the last one if the
  // end was reached.
  // After a click on the navigation, the chosen section stays marked even if it
  // cannot scroll to the top (the ones at the end), until the page moves.
  let atEnd = false;
  let pinned = null;
  for (const ev of ["wheel", "touchstart", "keydown"]) {
    addEventListener(
      ev,
      () => {
        if (pinned) {
          pinned = null;
          markActive();
        }
      },
      { passive: true }
    );
  }
  const markActive = () => {
    const line = innerHeight * 0.35;
    let act = null;
    let actTop = -Infinity;
    for (const s of sections) {
      const top = s.getBoundingClientRect().top;
      if (top <= line && top > actTop + 1) {
        act = s;
        actTop = top;
      }
    }
    if (atEnd && sections.length) act = sections[sections.length - 1];
    if (pinned) act = pinned;
    for (const a of links) {
      const on = !!act && a.dataset.nav === act.id;
      a.classList.toggle("is-active", on);
      if (on) a.setAttribute("aria-current", "location");
      else a.removeAttribute("aria-current");
    }
  };
  if ("IntersectionObserver" in window && sections.length) {
    const thresholds = [0, 0.1, 0.25, 0.5, 0.75, 1];
    const io = new IntersectionObserver(markActive, { rootMargin: "0px 0px -60% 0px", threshold: thresholds });
    for (const s of sections) io.observe(s);
    const end = el("span", "sr");
    end.setAttribute("aria-hidden", "true");
    main.append(end);
    new IntersectionObserver((entries) => {
      atEnd = entries.some((x) => x.isIntersecting);
      markActive();
    }).observe(end);
  }

  // ------------------------------------------------ filter by epic
  const scoped = ["your-turn", "problems", "what-it-looks-like"]
    .map((id) => doc.getElementById(id))
    .filter(Boolean);
  const navCount = new Map(
    links.map((a) => [a.dataset.nav, a.querySelector(".n")]).filter(([, n]) => n)
  );
  const original = new Map();
  for (const sec of scoped) {
    const c = sec.querySelector(".block-head .count");
    original.set(sec.id, { head: c ? c.textContent : "", nav: navCount.get(sec.id)?.textContent ?? "" });
  }
  const fold = doc.querySelector(".fold-todos");
  let foldWasOpen = false;
  const allBtn = side.querySelector(".epic-all");
  const sectionsBox = doc.querySelector(".sections");
  const filterBar = el("div", "filter-bar");
  filterBar.hidden = true;
  filterBar.setAttribute("role", "status");
  const filterText = el("p");
  const clearBtn = el("button", null, "Clear filter");
  clearBtn.type = "button";
  filterBar.append(filterText, clearBtn);
  if (sectionsBox) sectionsBox.prepend(filterBar);
  let current = null;
  const epicBtns = [];
  const setFilter = (id, title) => {
    current = id;
    doc.body.classList.toggle("filtering", !!id);
    for (const b of epicBtns) b.setAttribute("aria-pressed", String(b.dataset.id === id));
    if (allBtn) allBtn.setAttribute("aria-pressed", String(!id));
    if (fold) {
      if (id && !foldWasOpen && !fold.dataset.forced) {
        foldWasOpen = fold.open;
        fold.dataset.forced = "1";
        fold.open = true;
      } else if (!id && fold.dataset.forced) {
        fold.open = foldWasOpen;
        delete fold.dataset.forced;
        foldWasOpen = false;
      }
    }
    for (const sec of scoped) {
      let n = 0;
      for (const it of sec.querySelectorAll("[data-epic]")) {
        const show = !id || it.dataset.epic.split(" ").includes(id);
        it.hidden = !show;
        if (show) n++;
      }
      const o = original.get(sec.id);
      const head = sec.querySelector(".block-head .count");
      if (head) head.textContent = id ? String(n) : o.head;
      const nav = navCount.get(sec.id);
      if (nav) nav.textContent = id ? String(n) : o.nav;
      let empty = sec.querySelector(".filter-empty");
      if (id && n === 0 && sec.querySelector("[data-epic]")) {
        if (!empty) {
          empty = el("p", "filter-empty", "Nothing from this epic here.");
          sec.append(empty);
        }
        empty.hidden = false;
      } else if (empty) {
        empty.hidden = true;
      }
    }
    filterBar.hidden = !id;
    if (id) {
      filterText.replaceChildren("Showing only ", el("b", null, title));
    }
  };
  for (const li of side.querySelectorAll(".epic[data-epic-row]")) {
    const inner = li.querySelector(".epic-in");
    const id = li.dataset.epicRow;
    const title = li.querySelector(".epic-name").textContent.trim();
    const b = el("button", "epic-btn");
    b.type = "button";
    b.dataset.id = id;
    b.setAttribute("aria-pressed", "false");
    b.append(...inner.childNodes);
    inner.replaceWith(b);
    epicBtns.push(b);
    b.addEventListener("click", () => {
      setFilter(current === id ? null : id, title);
      if (panelOpen) {
        closePanel(false);
        // To the results: the filter notice stays right below the bar.
        if (sectionsBox) sectionsBox.scrollIntoView({ block: "start" });
        clearBtn.focus({ preventScroll: true });
      }
    });
  }
  if (allBtn) allBtn.addEventListener("click", () => setFilter(null));
  clearBtn.addEventListener("click", () => setFilter(null));
})();
