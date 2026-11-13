// kontext ui, the tree: draws /api/tree (what `kontext tree --json` gives)
// and reads it again every 5 seconds. It only reads.
(function () {
  "use strict";
  var POLL_MS = 5000;
  var MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  var LABEL = { done: "done", in_progress: "started", pending: "not started" };
  var SCOPE = { not_signed: "not signed", dropping: "dropping, waiting for you" };
  var last = "";

  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }

  function monthDay(iso) {
    var m = /^(\d{4})-(\d{2})-(\d{2})/.exec(iso || "");
    if (!m) return iso || "";
    return MONTHS[Number(m[2]) - 1] + " " + Number(m[3]);
  }

  function mark(progress, you) {
    var m = el("span", "mk " + progress + (you ? " you" : ""));
    m.setAttribute("role", "img");
    m.setAttribute("aria-label", LABEL[progress] || progress);
    return m;
  }

  function count(epic) {
    if (epic.features) return epic.features.done + "/" + epic.features.total + " features";
    return epic.tasks.done + "/" + epic.tasks.total + " tasks";
  }

  // A feature or epic outside the signed scope waits for the owner too.
  function waits(x) {
    return x.waiting_for_you || !!x.scope;
  }

  function feature(f) {
    var li = el("li", f.scope === "dropping" ? "dropping" : null);
    var line = el("div", "line");
    line.appendChild(mark(f.progress, waits(f)));
    line.appendChild(el("span", "title", f.title));
    var notes = [];
    if (f.scope) notes.push(el("span", "you", SCOPE[f.scope] || f.scope));
    if (f.status === "to validate") notes.push(el("span", "", "to validate"));
    if (f.waiting_for_you) notes.push(el("span", "you", "waiting for you"));
    if (notes.length) {
      var n = el("span", "note");
      notes.forEach(function (x, i) {
        if (i) n.appendChild(document.createTextNode(" · "));
        n.appendChild(x);
      });
      line.appendChild(n);
    }
    li.appendChild(line);
    if (f.acceptance && f.progress !== "done") {
      li.appendChild(el("p", "acc", "When it works: " + f.acceptance));
    }
    li.title = f.id;
    return li;
  }

  function epic(e) {
    var div = el("div", "epic" + (e.closed ? " closed" : ""));
    var line = el("div", "line");
    line.appendChild(mark(e.progress, !!e.scope));
    line.appendChild(el("span", "title", e.title));
    if (e.feature_list.length && !e.closed) {
      var t = el("span", "tally");
      t.setAttribute("aria-hidden", "true");
      e.feature_list.forEach(function (f) { t.appendChild(mark(f.progress, waits(f))); });
      line.appendChild(t);
    }
    var c = el("span", "count tab", count(e));
    if (e.scope) {
      c.appendChild(document.createTextNode(" · "));
      c.appendChild(el("span", "you", SCOPE[e.scope] || e.scope));
    }
    line.appendChild(c);
    div.appendChild(line);
    div.title = e.id + " · " + e.name;
    if (e.reason) div.appendChild(el("p", "reason", e.reason));
    if (!e.closed && e.feature_list.length) {
      var ul = el("ul", "features");
      e.feature_list.forEach(function (f) { ul.appendChild(feature(f)); });
      div.appendChild(ul);
    }
    return div;
  }

  function group(title, now) {
    var sec = el("section", "group" + (now ? " now" : ""));
    sec.appendChild(el("h2", "", title));
    var body = el("div", "body");
    sec.appendChild(body);
    return { sec: sec, body: body };
  }

  function audit(u) {
    if (!u || !u.count) return null;
    var p = el("p", "audit");
    var b = el("b", "tab", u.count + " of your messages");
    p.appendChild(b);
    p.appendChild(document.createTextNode(
      u.since ? " not audited since " + monthDay(u.since) : " never audited"));
    return p;
  }

  function draw(t) {
    document.getElementById("project").textContent = t.project;
    document.title = t.project + " · kontext";
    var s = document.getElementById("summary");
    s.textContent = "";
    var head = t.features
      ? [t.features.done + "/" + t.features.total, " features done"]
      : [t.epics.done + "/" + t.epics.total, " epics closed"];
    s.appendChild(el("b", "", head[0]));
    s.appendChild(document.createTextNode(head[1]));
    // The waves are kontext plan's: only open epics have one, wave 1 is now.
    var rest = [];
    if (t.waves.length) {
      rest.push(t.waves.length + (t.waves.length === 1 ? " wave" : " waves") + " to go");
    }
    if (t.no_wave.length) rest.push(t.no_wave.length + " with no wave");
    if (!t.waves.length && !t.no_wave.length && t.done.length) rest.push("all done");
    if (rest.length) s.appendChild(document.createTextNode(" · " + rest.join(" · ")));
    // The signed scope (only with a signed plan).
    var sc = document.getElementById("scope");
    sc.textContent = "";
    sc.hidden = true;
    if (t.scope && t.scope.pending) {
      s.appendChild(document.createTextNode(" · "));
      s.appendChild(el("span", "waits", "1 scope change waiting for you"));
      var parts = [];
      if (t.scope.pending.enters.length) parts.push(["enters ", t.scope.pending.enters.join(", ")]);
      if (t.scope.pending.leaves.length) parts.push(["leaves ", t.scope.pending.leaves.join(", ")]);
      parts.forEach(function (p, i) {
        if (i) sc.appendChild(document.createTextNode(" · "));
        sc.appendChild(document.createTextNode(p[0]));
        sc.appendChild(el("b", "", p[1]));
      });
      sc.hidden = !parts.length;
    } else if (t.scope) {
      s.appendChild(document.createTextNode(" · plan signed " + t.scope.signed_on));
    }

    var book = document.getElementById("book");
    book.textContent = "";
    if (!t.done.length && !t.waves.length && !t.no_wave.length) {
      var g0 = group("No epics", false);
      g0.body.appendChild(el("p", "empty",
        "Nothing planned yet. Add the first epic with kontext epic add \"<title>\" --goal <G>."));
      book.appendChild(g0.sec);
    }
    if (t.done.length) {
      var gd = group("Done", false);
      t.done.forEach(function (e) { gd.body.appendChild(epic(e)); });
      book.appendChild(gd.sec);
    }
    t.waves.forEach(function (w) {
      var g = group("Wave " + w.n, w.n === 1);
      w.epics.forEach(function (e) { g.body.appendChild(epic(e)); });
      book.appendChild(g.sec);
    });
    if (t.no_wave.length) {
      var gn = group("No wave", false);
      t.no_wave.forEach(function (e) { gn.body.appendChild(epic(e)); });
      book.appendChild(gn.sec);
    }
    if (t.not_placed) {
      var np = t.not_placed;
      var gi = group("Not placed", false);
      var a = audit(np.unaudited);
      if (a) gi.body.appendChild(a);
      if (!np.ideas.length) {
        gi.body.appendChild(el("p", "empty", "Every idea of yours has a feature, a task or a decision."));
      } else {
        var ul = el("ul", "ideas");
        np.ideas.forEach(function (i) {
          var li = el("li");
          li.appendChild(el("span", "words", "“" + (i.quote || i.idea) + "”"));
          var meta = [];
          if (i.said) meta.push(monthDay(i.said));
          if (i.cites) meta.push(i.cites);
          if (meta.length) li.appendChild(el("span", "meta", meta.join(" · ")));
          li.title = i.id;
          ul.appendChild(li);
        });
        gi.body.appendChild(ul);
      }
      book.appendChild(gi.sec);
    }
  }

  function live(text, bad) {
    var p = document.getElementById("live");
    p.textContent = text;
    p.className = bad ? "bad" : "";
  }

  function load() {
    fetch("/api/tree", { cache: "no-store" })
      .then(function (r) {
        return r.json().then(function (v) {
          if (!r.ok) throw new Error(v && v.error ? v.error : "status " + r.status);
          return v;
        });
      })
      .then(function (t) {
        var key = JSON.stringify(t);
        if (key !== last) {
          last = key;
          draw(t);
        }
        live("Updated " + new Date().toLocaleTimeString() + (streaming ? " · live" : " · reads again every 5 seconds"), false);
      })
      .catch(function (e) {
        live("Could not read the ledger (" + e.message + "). Trying again in 5 seconds.", true);
      });
  }

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
