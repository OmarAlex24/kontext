// kontext ui: reads /api/state every 5 s and draws. All the text goes in with
// textContent: nothing from the ledger is read as HTML.
(function () {
  "use strict";

  var POLL_MS = 5000;
  var KINDS = [
    ["charter", "Charter"], ["decision", "Decision"], ["question", "Question"],
    ["fact", "Fact"], ["task", "Task"], ["journal", "Journal"]
  ];
  var READER_COLORS = 6;

  var state = null;       // the last /api/state reply
  var lastText = "";      // so as not to redraw if nothing changed
  var extra = [];         // rows from earlier pages already loaded
  var nextBefore = null;
  var open = {};          // id -> open detail (JSON of /api/entry)
  var colorOf = {};       // reader -> color index
  var rendered = false;

  function $(id) { return document.getElementById(id); }

  // On narrow screens Fingerprint and Wrote are hidden: the lines and the
  // details only span the columns that are visible.
  var narrow = window.matchMedia("(max-width: 640px)");
  function visibleCols() { return narrow.matches ? 4 : 6; }
  narrow.addEventListener("change", function () { render(); });

  function h(tag, attrs) {
    var el = document.createElement(tag);
    if (attrs) {
      for (var k in attrs) {
        if (!Object.prototype.hasOwnProperty.call(attrs, k)) continue;
        var v = attrs[k];
        if (v === null || v === undefined || v === false) continue;
        if (k === "class") el.className = v;
        else if (k === "text") el.textContent = v;
        else if (k === "style") el.setAttribute("style", v);
        else if (k.slice(0, 2) === "on") el.addEventListener(k.slice(2), v);
        else el.setAttribute(k, v === true ? "" : v);
      }
    }
    for (var i = 2; i < arguments.length; i++) {
      var c = arguments[i];
      if (c === null || c === undefined || c === false) continue;
      if (Array.isArray(c)) c.forEach(function (x) { if (x) el.appendChild(typeof x === "string" ? document.createTextNode(x) : x); });
      else el.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
    }
    return el;
  }

  function clear(el) { while (el.firstChild) el.removeChild(el.firstChild); return el; }
  function plural(n, one, many) { return n + " " + (n === 1 ? one : many); }
  function rc(reader) { return "--rc: var(--r" + (colorOf[reader] % READER_COLORS) + ")"; }

  function since(text) {
    if (!text) return null;
    var t = Date.parse(text.replace(" UTC", "Z").replace(" ", "T"));
    if (isNaN(t)) return text;
    var m = Math.floor((Date.now() - t) / 60000);
    if (m < 1) return "less than 1 min ago";
    if (m < 60) return m + " min ago";
    var hr = Math.floor(m / 60);
    if (hr < 48) return hr + " h ago";
    return Math.floor(hr / 24) + " days ago";
  }

  // ------------------------------------------------------------ arriba
  function renderTop(s) {
    document.title = s.project + " · kontext";
    $("project").textContent = s.project;
    var st = s.status, chips = clear($("chips"));
    var chainOk = st.chains_ok;
    chips.appendChild(h("li", { class: "chip" + (chainOk ? "" : " bad"), title: chainOk ? "kontext verify found no problems" : st.chain_problems.join("\n") },
      h("span", { class: "dot", "aria-hidden": "true" }),
      h("b", { text: chainOk ? "Chain OK" : "Chain broken" }),
      chainOk ? null : h("span", { text: "(" + st.chain_problems.length + ")" })));
    chips.appendChild(h("li", { class: "chip" },
      h("span", { text: "Último sync:" }),
      h("b", { text: st.last_sync ? (since(st.last_sync) || st.last_sync) : (st.remote ? "never" : "no remote") })));
    var names = s.writers.map(function (w) { return w.name; });
    chips.appendChild(h("li", { class: "chip", title: names.join(", ") },
      h("b", { text: plural(names.length, "writer", "writers") }),
      h("span", { class: "muted", text: names.join(", ") })));
  }

  function renderLegend(s) {
    var l = clear($("legend"));
    KINDS.forEach(function (k) {
      l.appendChild(h("li", null, h("span", { class: "sq k-" + k[0], "aria-hidden": "true" }), k[1]));
    });
    l.appendChild(h("li", null, h("span", { class: "sq hollow k-decision", "aria-hidden": "true" }), "Unreviewed proposal"));
    s.readers.forEach(function (r) {
      if (!r.has_cursor) return;
      l.appendChild(h("li", { style: rc(r.name) }, h("span", { class: "bar", style: "background: var(--rc)", "aria-hidden": "true" }), "New for " + r.name));
    });
  }

  // ------------------------------------------------------------ ledger
  function entryCell(r) {
    var title = h("button", {
      type: "button", class: "entry-title", "aria-expanded": open[r.id] ? "true" : "false",
      "aria-controls": "detail-" + r.id,
      onclick: function () { toggle(r.id); }
    }, h("span", { class: "t", text: r.title }), h("span", { class: "id", text: r.id }));
    var cell = h("td", { class: "c-entry" + (r.struck ? " struck" : "") }, title);
    if (r.note) cell.appendChild(h("p", { class: "note", text: r.note }));
    if (r.warning) cell.appendChild(h("p", { class: "note warn", text: r.warning }));
    cell.appendChild(h("p", { class: "note m-actor" }, r.actor_name + " · " + r.writer, r.stamp ? h("span", { class: "stamp", text: r.stamp }) : null));
    return cell;
  }

  function actorCell(r) {
    var who = h("span", { class: r.human ? null : "agent", text: r.actor_name });
    var cell = h("td", { class: "c-actor", title: r.actor }, h("div", { class: "who" }, who),
      h("span", { class: "machine", text: (r.role ? r.role + " · " : r.human ? "" : "agent · ") + r.writer }));
    if (r.stamp) cell.appendChild(h("span", { class: "stamp", text: r.stamp }));
    return cell;
  }

  function gutterCell(active, ending) {
    var td = h("td", { class: "c-gutter", "aria-hidden": "true" });
    active.forEach(function (name, k) {
      if (!name) return;
      td.appendChild(h("span", { class: "gbar", style: rc(name) + "; right: " + (6 + k * 7) + "px" }));
    });
    if (ending) {
      td.appendChild(h("span", { class: "gend", style: rc(ending.name) + "; right: " + (6 + ending.slot * 7) + "px" }));
    }
    return td;
  }

  function renderLedger(s) {
    var rows = s.ledger.entries.concat(extra);
    var readers = s.readers.filter(function (r) { return r.has_cursor; });
    var slots = readers.map(function (r) { return r.name; });
    $("ledger").style.setProperty("--gutter", Math.max(1.5, (12 + slots.length * 7) / 16) + "rem");
    // Where each reader's new entries end: below their oldest unread entry
    // ; at the very top if they are up to date.
    var marks = {};  // row index -> readers whose line goes before it
    var tailOpen = [];
    readers.forEach(function (r, slot) {
      var idx;
      if (!r.first_new) idx = 0;
      else {
        idx = -1;
        for (var i = 0; i < rows.length; i++) if (rows[i].id === r.first_new) { idx = i + 1; break; }
      }
      if (idx < 0) { tailOpen.push(slot); return; }
      (marks[idx] = marks[idx] || []).push({ reader: r, slot: slot });
    });
    var activeSet = slots.slice();
    var body = clear($("rows"));
    function addCursorRows(i) {
      (marks[i] || []).forEach(function (m) {
        activeSet[m.slot] = null;
        var label = r_label(m.reader);
        body.appendChild(h("tr", { class: "cursor", style: rc(m.reader.name) },
          h("td", { class: "cursor-cell", colspan: String(visibleCols() - 1) },
            h("span", { class: "cursor-line", "aria-hidden": "true" }),
            h("span", { class: "cursor-label", text: label })),
          gutterCell(activeSet.slice(), { name: m.reader.name, slot: m.slot })));
      });
    }
    for (var i = 0; i < rows.length; i++) {
      addCursorRows(i);
      var r = rows[i];
      var unread = activeSet.some(function (x) { return x; });
      var tr = h("tr", { id: "row-" + r.id, class: unread ? "unread" : null },
        h("td", { class: "c-n", title: r.writer + " · " + r.when, text: String(r.n) }),
        h("td", { class: "c-hash" }, h("span", { class: "hash", text: r.hash }), h("span", { class: "prev", text: "← " + r.prev })),
        h("td", { class: "c-kind" }, h("span", { class: "kind" },
          h("span", { class: "sq k-" + r.category + (r.hollow ? " hollow" : ""), "aria-hidden": "true" }),
          h("span", { class: "kl", text: r.hollow ? "Proposal" : r.category_label }),
          h("span", { class: "sr", text: r.kind_label }))),
        entryCell(r),
        actorCell(r),
        gutterCell(activeSet.slice()));
      body.appendChild(tr);
      if (open[r.id]) body.appendChild(detailRow(r.id, open[r.id], activeSet.slice()));
    }
    addCursorRows(rows.length);
    var shown = rows.length, total = s.ledger.total;
    $("ledger-count").textContent = shown < total ? shown + " of " + total + " entries" : plural(total, "entry", "entries");
    var more = $("more");
    more.hidden = !nextBefore;
    if (tailOpen.length && nextBefore) {
      more.textContent = "See earlier entries (there are more unread)";
    } else {
      more.textContent = "Ver entradas anteriores";
    }
  }

  function r_label(r) {
    if (!r.first_new) return "Read up to here: " + r.name + " · up to date";
    return "Read up to here: " + r.name + " · " + plural(r.new, "new", "new") + " above";
  }

  function detailRow(id, d, active) {
    var e = d.entry || {};
    var dl = h("dl");
    function add(k, v) { if (v === null || v === undefined || v === "" || (Array.isArray(v) && !v.length)) return; dl.appendChild(h("dt", { text: k })); dl.appendChild(h("dd", null, v)); }
    add("Status", d.status_label ? d.status_label + (d.counts === false ? " · does not count" : "") : null);
    add("Why", e.why);
    add("Detalle", e.body);
    add("When", e.hlc);
    add("Writer", e.writer + " · n " + e.n);
    add("Fingerprint", e.hash);
    add("Anterior", e.prev);
    add("Actor", e.actor ? (e.actor.type + ":" + e.actor.name + (e.actor.role ? " (" + e.actor.role + ")" : "") + (e.actor.model ? " · " + e.actor.model : "")) : null);
    if (e.tags && e.tags.length) add("Etiquetas", e.tags.join(", "));
    if (e.fields && Object.keys(e.fields).length) add("Campos", JSON.stringify(e.fields, null, 2));
    if (d.refs_rows && d.refs_rows.length) {
      add("Relacionadas", h("ul", null, d.refs_rows.map(function (x) {
        return h("li", null, h("a", { href: "#row-" + x.id, onclick: function (ev) { jump(ev, x.id); }, text: x.id }), " " + x.title);
      })));
    }
    if (e.sig) add("Signature", "signed with SSH");
    return h("tr", { class: "detail", id: "detail-" + id }, h("td", { colspan: String(visibleCols() - 1) }, dl), gutterCell(active));
  }

  function toggle(id) {
    if (open[id]) { delete open[id]; render(); focusRow(id); return; }
    fetch("/api/entry/" + encodeURIComponent(id), { cache: "no-store" })
      .then(function (r) { return r.json(); })
      .then(function (d) { open[id] = d; render(); focusRow(id); })
      .catch(function () { setLive("Could not read " + id, true); });
  }

  function focusRow(id) {
    var b = document.querySelector("#row-" + cssId(id) + " .entry-title");
    if (b) b.focus();
  }
  function cssId(id) { return id.replace(/[^A-Za-z0-9_-]/g, "\\$&"); }

  function jump(ev, id) {
    var row = document.getElementById("row-" + id);
    if (!row) return;
    ev.preventDefault();
    row.scrollIntoView({ block: "center", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    row.classList.add("hit");
    setTimeout(function () { row.classList.remove("hit"); }, 1400);
    var b = row.querySelector(".entry-title");
    if (b) b.focus({ preventScroll: true });
  }

  function idLink(id) {
    return h("a", { href: "#row-" + id, onclick: function (ev) { jump(ev, id); }, text: id });
  }

  // ------------------------------------------------------------ lectores
  function renderReaders(s) {
    var box = clear($("readers"));
    s.readers.forEach(function (r) {
      var c = r.card || {};
      var title = r.is_you ? "For you (" + r.name + ")" : r.role === "pm" ? "Project manager" : r.role === "epic" ? "Coordinator of " + r.name : r.name;
      var count = r.has_cursor ? plural(r.new, "new", "new") : "everything is new";
      var card = h("article", { class: "card", style: rc(r.name) },
        h("div", { class: "card-head" }, h("span", { class: "bar", "aria-hidden": "true" }), h("h3", { text: title }), h("span", { class: "count", text: count })));
      card.appendChild(h("p", { class: "sub", text: r.has_cursor ? "Read " + (since(r.last_read) || "at some point") + " · cursor " + cursorText(r.cursor) : "No cursor yet" }));
      if (r.role === "pm") {
        card.appendChild(h("p", { class: "lbl", text: "Épicas" }));
        card.appendChild(list(c.epics || [], function (l) { return l; }, "No epics"));
        if (c.questions && c.questions.length) {
          card.appendChild(h("p", { class: "lbl", text: "Le preguntan" }));
          card.appendChild(list(c.questions, function (q) { return [idLink(q.id), " " + q.text]; }));
        }
      } else if (r.role === "epic") {
        if (c.counts) {
          card.appendChild(h("div", { class: "counts" }, c.counts.map(function (x) { return h("div", null, h("b", { text: String(x.n) }), x.label); })));
        } else {
          card.appendChild(h("p", { class: "summary muted", text: "The epic is no longer in force" }));
        }
        if (c.questions && c.questions.length) {
          card.appendChild(h("p", { class: "lbl", text: "Le preguntan" }));
          card.appendChild(list(c.questions, function (q) { return [idLink(q.id), " " + q.text]; }));
        }
      } else {
        if (c.summary) card.appendChild(h("p", { class: "summary", text: c.summary.replace(/^Resumen: /, "") }));
        card.appendChild(h("p", { class: "lbl", text: "Waiting for your answer" }));
        card.appendChild(list(c.waiting || [], function (w) { return [idLink(w.id), " " + w.text]; }, "Nothing waits for your answer"));
      }
      box.appendChild(card);
    });
  }

  function cursorText(cur) {
    var parts = [];
    for (var w in cur) if (Object.prototype.hasOwnProperty.call(cur, w)) parts.push(w + " " + cur[w]);
    return parts.join(", ") || "empty";
  }

  function list(items, fn, empty) {
    if (!items.length) return h("p", { class: "summary muted", text: empty || "Nothing" });
    return h("ul", null, items.map(function (x) { return h("li", null, fn(x)); }));
  }

  // ------------------------------------------------------------ alertas
  function renderAlerts(s) {
    var box = clear($("alerts"));
    if (!s.alerts.length) { box.appendChild(h("p", { class: "empty", text: "No alerts: nothing drifts from the charter." })); return; }
    box.appendChild(h("ul", { class: "alerts" }, s.alerts.map(function (a) {
      var ids = (a.ids || []).filter(function (x, i, arr) { return arr.indexOf(x) === i; });
      var links = [];
      ids.forEach(function (id, i) { if (i) links.push(", "); links.push(idLink(id)); });
      return h("li", null, h("span", { class: "cat", text: a.label }), a.text, links.length ? h("span", { class: "muted" }, " · ", links) : null);
    })));
  }

  // ------------------------------------------------------------ charter
  function renderCharter(s) {
    var box = clear($("charter"));
    var c = s.charter;
    if (!c) { box.appendChild(h("p", { class: "empty", text: "No charter. Write it with kontext charter and sign it with kontext sign." })); return; }
    var f = c.fields || {};
    var card = h("div", { class: "charter" },
      h("p", { class: "obj", text: f.objective || c.title }),
      h("p", { class: "lbl", text: c.status === "signed" ? "Signed by " + (c.signed_by === s.you ? "you" : (c.signed_by || "an owner")) : "Unsigned" }));
    if (s.goals.length) {
      card.appendChild(h("p", { class: "lbl", text: "Goals" }));
      card.appendChild(h("ul", null, s.goals.map(function (g) { return h("li", null, idLink(g.id), " " + g.title + (g.status === "unsigned" ? " (unsigned)" : "")); })));
    }
    if (f.non_goals && f.non_goals.length) {
      card.appendChild(h("p", { class: "lbl", text: "Not doing" }));
      card.appendChild(h("ul", null, f.non_goals.map(function (n) { return h("li", { text: n }); })));
    }
    if (s.decisions.length) {
      card.appendChild(h("p", { class: "lbl", text: "Decisions in force" }));
      card.appendChild(h("ul", null, s.decisions.map(function (d) { return h("li", null, idLink(d.id), " " + d.title); })));
    }
    box.appendChild(card);
  }

  // ------------------------------------------------------------ tableros
  function renderBoards(s) {
    var box = clear($("boards"));
    if (!s.epics.length) { box.appendChild(h("p", { class: "empty", text: "No epics. Create them with kontext epic add." })); return; }
    s.epics.forEach(function (e) {
      var cols = h("div", { class: "cols" });
      e.columns.forEach(function (col) {
        var tasks = e.tasks.filter(function (t) { return t.status === col.status; });
        var c = h("div", { class: "col" }, h("h4", null, col.label, h("span", { text: String(tasks.length) })));
        if (!tasks.length) c.appendChild(h("p", { class: "none", text: "Ninguna" }));
        tasks.forEach(function (t) {
          var line = t.drift || (t.line === col.status ? "" : (t.line || ""));
          c.appendChild(h("div", { class: "task" + (t.drift ? " drift" : "") },
            h("div", { class: "tid" }, idLink(t.id)),
            h("div", { class: "tt", text: t.title }),
            line ? h("div", { class: "tl", text: line }) : null));
        });
        cols.appendChild(c);
      });
      box.appendChild(h("article", { class: "board" },
        h("div", { class: "board-head" }, h("h3", { text: e.title }), h("p", { text: "epic:" + e.name + " · sirve a " + e.goals.join(", ") })),
        cols));
    });
  }

  // ------------------------------------------------------------ ciclo
  function render() {
    if (!state) return;
    var y = window.scrollY;
    renderTop(state);
    renderLegend(state);
    renderLedger(state);
    renderReaders(state);
    renderAlerts(state);
    renderCharter(state);
    renderBoards(state);
    window.scrollTo(0, y);
  }

  function setLive(text, bad) {
    var l = $("live");
    l.textContent = text;
    l.className = bad ? "bad" : "";
  }

  function assignColors(s) {
    s.readers.forEach(function (r) {
      if (!(r.name in colorOf)) colorOf[r.name] = Object.keys(colorOf).length;
    });
  }

  function load() {
    var limit = Math.max(100, state ? state.ledger.entries.length : 0);
    fetch("/api/state?limit=" + limit, { cache: "no-store" })
      .then(function (r) { if (!r.ok) throw new Error(String(r.status)); return r.text(); })
      .then(function (text) {
        var now = new Date().toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });
        setLive((streaming ? "Live" : "Updates every 5 s") + " · last read " + now);
        if (text === lastText) return;
        lastText = text;
        state = JSON.parse(text);
        if (!extra.length) nextBefore = state.ledger.next_before;
        assignColors(state);
        var first = !rendered;
        rendered = true;
        render();
        // A link to #row-<id> or to a section works even if the page
        // draws after loading.
        if (first && location.hash.length > 1) {
          var target = document.getElementById(decodeURIComponent(location.hash.slice(1)));
          if (target) target.scrollIntoView({ block: "start" });
        }
      })
      .catch(function () { setLive("No connection to kontext ui: is it still running? Retrying in 5 s.", true); });
  }

  $("more").addEventListener("click", function () {
    if (!nextBefore) return;
    fetch("/api/state?limit=100&before=" + encodeURIComponent(nextBefore), { cache: "no-store" })
      .then(function (r) { return r.json(); })
      .then(function (d) {
        extra = extra.concat(d.ledger.entries);
        nextBefore = d.ledger.next_before;
        render();
      })
      .catch(function () { setLive("Could not read the earlier entries", true); });
  });

  // Live: kontext ui says when the files change (and, with a server, when
  // another machine writes); the poll stays as the fallback.
  var streaming = false;
  if (window.EventSource) {
    var events = new EventSource("/api/events");
    events.addEventListener("open", function () { streaming = true; });
    events.addEventListener("error", function () { streaming = false; });
    events.addEventListener("change", load);
  }

  load();
  setInterval(load, POLL_MS);
})();
