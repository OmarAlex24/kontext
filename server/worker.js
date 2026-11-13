// kontext server: a relay of ledger lines (DESIGN §14).
//
// One SQLite-backed Durable Object per project stores each writer's ledger
// lines exactly as the binary wrote them, the cursor files, the versioned
// config files (project.toml, synonyms.toml) and the media (by sha256, chunked
// in rows), and hands them out. It never derives state: what counts in the
// ledger is the binary's job. What it checks is identity and the chain:
//
// - a line must continue the server's chain for its writer (`n` is the next
//   one and `prev` is the hash of the server's last line), so a fork is never
//   stored; a line the server already has (same `n`, same `hash`) is a no-op;
// - a token appends only to its own writer's file, and writes only its own
//   writer's cursor files; only an owner token (or the admin secret) writes
//   config.
//
// A hibernatable WebSocket per project announces new lines (the frontier, not
// the lines); clients pull what they lack.
//
// Plain JavaScript, no build step: `kontext server deploy` uploads this file
// as it is (it is embedded in the binary).

import { DurableObject } from "cloudflare:workers";

const PROTOCOL = 1;
const GENESIS = "0".repeat(64);
const PROJECT_RE = /^[A-Za-z0-9._-]{1,64}$/;
const WRITER_RE = /^[A-Za-z0-9_@-][A-Za-z0-9._@-]{0,127}$/;
const READER_RE = /^(?!\.)[\p{L}\p{N}:._@-]{1,128}$/u;
const HEX64_RE = /^[0-9a-f]{64}$/;
const MEDIA_RE = /^([0-9a-f]{64})\.(png|jpg|jpeg|webp|html)$/;
const CONFIG_FILES = ["project.toml", "synonyms.toml"];
const ROLES = ["owner", "pm", "coordinator", "implementer", "tester", "scout"];

// The longest ledger line (UTF-8 bytes) the server takes. The binary refuses
// to write a longer one in a project with a server (`MAX_LINE_BYTES` in
// src/server.rs, pinned to this one by a test), so every line it writes can
// be pushed. 4 MiB is about a thousand times the largest entry kontext writes
// (an entry carries text, and images go to media/), and it keeps a push of one
// such line, JSON-escaped (at most twice its size), far below the 100 MB
// request body and the 128 MB of memory of an isolate (Workers limits).
const MAX_LINE_BYTES = 4 * 1024 * 1024;
// A line longer than this is stored in rows of this many bytes, as media are
// (a row holds at most 8 MB); shorter ones in one row, as before.
const LINE_CHUNK = 512 * 1024;
// A push body: one batch of the binary (2 MiB of lines) or a single line of
// up to MAX_LINE_BYTES, JSON-escaped, with room to spare.
const MAX_PUSH_BODY = 2 * MAX_LINE_BYTES + 1024 * 1024;
const MAX_PUSH_LINES = 1000;
const PULL_MAX_LINES = 2000;
const PULL_MAX_BYTES = 4 * 1024 * 1024;
const MAX_CONFIG_BYTES = 256 * 1024;
const MAX_CURSOR_BYTES = 64 * 1024;
const MAX_MEDIA_BYTES = 32 * 1024 * 1024;
// A SQLite row in a Durable Object holds at most 8 MB; 1 MiB chunks stay far
// below it and keep each statement small.
const MEDIA_CHUNK = 1024 * 1024;

function json(body, status = 200, headers = {}) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json; charset=utf-8", ...headers },
  });
}

function error(status, code, message, extra = {}) {
  return json({ error: code, message, ...extra }, status);
}

const enc = new TextEncoder();

async function sha256Hex(data) {
  const bytes = typeof data === "string" ? enc.encode(data) : data;
  const d = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function sameSecret(a, b) {
  const [x, y] = await Promise.all([
    crypto.subtle.digest("SHA-256", enc.encode(a)),
    crypto.subtle.digest("SHA-256", enc.encode(b)),
  ]);
  return crypto.subtle.timingSafeEqual(x, y);
}

function b64url(bytes) {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/// A token: `kxt1.<base64url of its claims>.<secret>`. The claims are
/// `{p: project, w: writer, n: person, r: role, e?: epic, c?: [coordinator
/// names], x?: expiry in ms}`: the binary reads them without the network (to
/// pick the writer and to refuse, before writing, a line the server would
/// refuse). The server keeps only the sha256 and decides from its own row,
/// never from the claims.
function mintToken(project, spec) {
  const claims = { p: project, w: spec.writer, n: spec.person, r: spec.role };
  if (spec.epic) claims.e = spec.epic;
  if (spec.actors && spec.actors.length) claims.c = spec.actors;
  if (spec.expires) claims.x = spec.expires;
  const head = b64url(enc.encode(JSON.stringify(claims)));
  const secret = b64url(crypto.getRandomValues(new Uint8Array(32)));
  return `kxt1.${head}.${secret}`;
}

function bearer(req) {
  const h = req.headers.get("authorization") || "";
  const m = /^Bearer\s+(\S+)$/i.exec(h);
  return m ? m[1] : null;
}

async function readJson(req, max) {
  const text = await req.text();
  if (text.length > max) throw new HttpError(413, "too_large", `the body is over ${max} bytes`);
  try {
    return JSON.parse(text);
  } catch {
    throw new HttpError(400, "bad_json", "the body is not JSON");
  }
}

class HttpError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

/// What only an owner or the pm writes: the plan and the decisions.
const GOVERNANCE = ["charter", "goal", "epic", "feature", "scope", "sign", "accept", "reject", "decision", "supersede"];
/// What a coordinator hands out, and an implementer or tester may not.
const COORDINATION = ["task", "assign"];
/// All a scout writes.
const SCOUT_KINDS = ["fact", "journal", "question", "confirm", "proposal"];

/// Does this token's identity allow this line? `null` if it does, otherwise
/// why not (DESIGN §14.2). Only identity: who the actor is and which kinds
/// its role writes, never the project's state. The binary runs the same rules
/// before writing (`src/remote.rs`, `identity_problem`), so a refused line is
/// never in a ledger file; these are the backstop.
function actorAllowed(p, e) {
  if (p.admin || p.role === "owner") return null;
  const a = e.actor && typeof e.actor === "object" ? e.actor : {};
  const name = String(a.name || "");
  const role = a.role == null ? null : String(a.role);
  const fields = e.fields && typeof e.fields === "object" ? e.fields : {};
  // A fact replacing a stale fact (both ids `F-…`, read from the line, not
  // the state) is what any agent writes when it records a fact again.
  const refs = Array.isArray(e.refs) ? e.refs.map(String) : [];
  const factSupersede = e.kind === "supersede" && refs.length > 0 && refs.every((r) => r.startsWith("F-"));
  const kind = factSupersede ? "fact" : String(e.kind || "");
  if (a.type !== "agent") return `a ${p.role} token writes only as agents, not as ${a.type}:${name}`;
  if (p.role === "pm") return null;
  if (role === "pm") return `only a pm or owner token writes as role pm`;
  if (fields.approved_by != null) return `only a pm or owner token writes approved_by`;
  if (GOVERNANCE.includes(kind)) return `a ${p.role} token does not write ${kind} (only an owner or the pm)`;
  if (p.role === "coordinator") {
    const mine = p.actors || [];
    if (role === "coordinator" && !mine.includes(name)) {
      return `this token coordinates as ${mine.join(" or ")}, not as agent:${name}`;
    }
    if (name.startsWith("epic:") && !mine.includes(name)) {
      return `agent:${name} is another epic's coordinator; this token coordinates as ${mine.join(" or ")}`;
    }
    return null;
  }
  if (role === "coordinator" || name.startsWith("epic:")) {
    return `a ${p.role} token does not write as a coordinator (agent:${name})`;
  }
  if (COORDINATION.includes(kind)) return `a ${p.role} token does not write ${kind} (the coordinator does)`;
  if (p.role === "scout" && !SCOUT_KINDS.includes(kind)) {
    return `a scout token writes only ${SCOUT_KINDS.join(", ")}, not ${kind}`;
  }
  return null;
}

function parseActors(v) {
  if (!v) return [];
  try {
    const a = JSON.parse(v);
    return Array.isArray(a) ? a.map(String) : [];
  } catch {
    return [];
  }
}

function tokenRow(t) {
  return {
    id: t.id, person: t.person, role: t.role, epic: t.epic || null, actors: parseActors(t.actors),
    writer: t.writer, created: t.created, expires: t.expires || null, revoked: t.revoked || null,
    created_by: t.created_by || null,
  };
}

async function drain(req) {
  if (req.body && !req.bodyUsed) await req.arrayBuffer().catch(() => {});
}

export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    const parts = url.pathname.split("/").filter(Boolean);
    const fail = async (status, code, message) => {
      await drain(req);
      return error(status, code, message);
    };
    if (parts[0] !== "v1") return fail(404, "not_found", "kontext server: try /v1/info");
    if (parts.length === 2 && parts[1] === "info") {
      await drain(req);
      return json({ kontext_server: PROTOCOL });
    }
    if (parts[1] !== "p" || parts.length < 4) return fail(404, "not_found", "no such route");
    let project;
    try {
      project = decodeURIComponent(parts[2]);
    } catch {
      return fail(400, "bad_project", "invalid project name");
    }
    if (!PROJECT_RE.test(project)) {
      return fail(400, "bad_project", "a project name has letters, digits, - _ and .");
    }
    // Reject what is not a kontext client before it reaches (and is billed to)
    // the Durable Object.
    if (!bearer(req)) return fail(401, "unauthorized", "missing Authorization: Bearer <token>");
    const rest = "/" + parts.slice(3).join("/");
    const inner = new URL(rest + url.search, "https://do.invalid");
    const fwd = new Request(inner, req);
    fwd.headers.set("x-kontext-project", project);
    const stub = env.PROJECT.get(env.PROJECT.idFromName(project));
    return stub.fetch(fwd);
  },
};

export class Project extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    // WITHOUT ROWID tables keyed by what is looked up: each stored line is one
    // row written (no secondary index), which matters on the Free plan's
    // 100,000 rows written per day.
    this.sql.exec(`
      CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS lines(writer TEXT NOT NULL, n INTEGER NOT NULL, hash TEXT NOT NULL,
        line TEXT NOT NULL, PRIMARY KEY(writer, n)) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS tails(writer TEXT PRIMARY KEY, n INTEGER NOT NULL, hash TEXT NOT NULL) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS cursors(reader TEXT NOT NULL, writer TEXT NOT NULL, body TEXT NOT NULL,
        stamp INTEGER NOT NULL, seq INTEGER NOT NULL, PRIMARY KEY(reader, writer)) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS config(name TEXT PRIMARY KEY, version INTEGER NOT NULL, sha256 TEXT NOT NULL,
        body TEXT NOT NULL, by TEXT NOT NULL, at TEXT NOT NULL) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS line_chunks(writer TEXT NOT NULL, n INTEGER NOT NULL, i INTEGER NOT NULL,
        data BLOB NOT NULL, PRIMARY KEY(writer, n, i)) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS media(name TEXT PRIMARY KEY, sha256 TEXT NOT NULL, bytes INTEGER NOT NULL,
        chunks INTEGER NOT NULL, seq INTEGER NOT NULL) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS media_chunks(name TEXT NOT NULL, i INTEGER NOT NULL, data BLOB NOT NULL,
        PRIMARY KEY(name, i)) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS tokens(id TEXT PRIMARY KEY, hash TEXT NOT NULL, person TEXT NOT NULL,
        role TEXT NOT NULL, epic TEXT, actors TEXT, writer TEXT NOT NULL, created TEXT NOT NULL, expires INTEGER,
        revoked TEXT, created_by TEXT) WITHOUT ROWID;
    `);
  }

  // ---- small helpers ----

  meta(k) {
    const r = this.sql.exec("SELECT v FROM meta WHERE k = ?", k).toArray();
    return r.length ? r[0].v : null;
  }

  setMeta(k, v) {
    this.sql.exec("INSERT INTO meta(k, v) VALUES(?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v", k, String(v));
  }

  seq() {
    return Number(this.meta("seq") || 0);
  }

  bump() {
    const s = this.seq() + 1;
    this.setMeta("seq", s);
    return s;
  }

  tails() {
    const out = {};
    for (const r of this.sql.exec("SELECT writer, n, hash FROM tails ORDER BY writer").toArray()) {
      out[r.writer] = { n: r.n, hash: r.hash };
    }
    return out;
  }

  tail(writer) {
    const r = this.sql.exec("SELECT n, hash FROM tails WHERE writer = ?", writer).toArray();
    return r.length ? { n: r[0].n, hash: r[0].hash } : { n: 0, hash: GENESIS };
  }

  configVersions() {
    const out = {};
    for (const r of this.sql.exec("SELECT name, version, sha256 FROM config ORDER BY name").toArray()) {
      out[r.name] = { version: r.version, sha256: r.sha256 };
    }
    return out;
  }

  frontier() {
    return { seq: this.seq(), tails: this.tails(), config: this.configVersions() };
  }

  notify() {
    const msg = JSON.stringify({ type: "frontier", ...this.frontier() });
    for (const ws of this.ctx.getWebSockets()) {
      try {
        ws.send(msg);
      } catch {
        // A socket that is closing; the client reconnects and pulls.
      }
    }
  }

  async principal(req) {
    const tok = bearer(req);
    if (!tok) throw new HttpError(401, "unauthorized", "missing Authorization: Bearer <token>");
    if (this.env.ADMIN_SECRET && (await sameSecret(tok, this.env.ADMIN_SECRET))) {
      return { admin: true, role: "admin", person: "admin", writer: null, epic: null };
    }
    const hash = await sha256Hex(tok);
    const rows = this.sql.exec("SELECT * FROM tokens WHERE hash = ?", hash).toArray();
    if (!rows.length) throw new HttpError(401, "unauthorized", "unknown token for this project");
    const t = rows[0];
    if (t.revoked) throw new HttpError(401, "revoked", `token ${t.id} was revoked`);
    if (t.expires && Date.now() > t.expires) throw new HttpError(401, "expired", `token ${t.id} expired`);
    return {
      admin: false, id: t.id, role: t.role, person: t.person, writer: t.writer, epic: t.epic || null,
      actors: parseActors(t.actors),
    };
  }

  requireProject() {
    if (!this.meta("name")) throw new HttpError(404, "no_project", "this project does not exist on the server");
  }

  insertToken({ person, role, epic, actors, writer, expires, createdBy }) {
    const active = this.sql
      .exec("SELECT id FROM tokens WHERE writer = ? AND revoked IS NULL AND (expires IS NULL OR expires > ?)", writer, Date.now())
      .toArray();
    if (active.length) {
      throw new HttpError(409, "writer_taken", `writer ${writer} already has an active token (${active[0].id}); revoke it first`);
    }
    return { person, role, epic: epic || null, actors: actors || [], writer, expires: expires || null, createdBy };
  }

  async storeToken(spec, project) {
    const token = mintToken(project, spec);
    const id = "tok_" + b64url(crypto.getRandomValues(new Uint8Array(6)));
    const hash = await sha256Hex(token);
    this.sql.exec(
      "INSERT INTO tokens(id, hash, person, role, epic, actors, writer, created, expires, revoked, created_by) VALUES(?,?,?,?,?,?,?,?,?,NULL,?)",
      id, hash, spec.person, spec.role, spec.epic, spec.actors.length ? JSON.stringify(spec.actors) : null,
      spec.writer, new Date().toISOString(), spec.expires, spec.createdBy,
    );
    return {
      token, id, person: spec.person, role: spec.role, epic: spec.epic, actors: spec.actors, writer: spec.writer,
      expires: spec.expires,
    };
  }

  /// A line as received. One longer than LINE_CHUNK bytes goes in
  /// `line_chunks`, by its UTF-8 bytes (so it comes back byte for byte), with
  /// an empty `line` in `lines` (a real line is never empty).
  storeLine(writer, n, hash, line, bytes) {
    if (bytes.length <= LINE_CHUNK) {
      this.sql.exec("INSERT INTO lines(writer, n, hash, line) VALUES(?, ?, ?, ?)", writer, n, hash, line);
      return;
    }
    for (let i = 0; i * LINE_CHUNK < bytes.length; i++) {
      const part = bytes.slice(i * LINE_CHUNK, Math.min(bytes.length, (i + 1) * LINE_CHUNK));
      this.sql.exec("INSERT OR REPLACE INTO line_chunks(writer, n, i, data) VALUES(?, ?, ?, ?)", writer, n, i, part.buffer);
    }
    this.sql.exec("INSERT INTO lines(writer, n, hash, line) VALUES(?, ?, ?, '')", writer, n, hash);
  }

  chunkedLine(writer, n) {
    const parts = this.sql
      .exec("SELECT data FROM line_chunks WHERE writer = ? AND n = ? ORDER BY i", writer, n)
      .toArray()
      .map((r) => new Uint8Array(r.data));
    const out = new Uint8Array(parts.reduce((a, p) => a + p.length, 0));
    let off = 0;
    for (const p of parts) {
      out.set(p, off);
      off += p.length;
    }
    return new TextDecoder().decode(out);
  }

  // ---- routes ----

  async fetch(req) {
    try {
      return await this.route(req);
    } catch (e) {
      // Read what the client sent before answering: a response to a request
      // whose body was never read makes the runtime reset the connection, and
      // the client sees a network error instead of this answer.
      await drain(req);
      if (e instanceof HttpError) return error(e.status, e.code, e.message);
      return error(500, "internal", String(e && e.message ? e.message : e));
    }
  }

  async route(req) {
    const url = new URL(req.url);
    const project = req.headers.get("x-kontext-project");
    const parts = url.pathname.split("/").filter(Boolean);
    const m = req.method;
    const p = await this.principal(req);

    if (parts[0] === "create" && m === "POST") return this.create(req, p, project);
    this.requireProject();

    if (parts[0] === "ws") return this.websocket(req);
    if (parts[0] === "whoami" && m === "GET") {
      return json({
        project, role: p.role, person: p.person, epic: p.epic, actors: p.actors || [], writer: p.writer, admin: p.admin,
        protocol: PROTOCOL,
      });
    }
    if (parts[0] === "frontier" && m === "GET") return json(this.frontier());
    if (parts[0] === "push" && m === "POST") return this.push(req, p);
    if (parts[0] === "pull" && m === "POST") return this.pull(req);
    if (parts[0] === "cursors" && m === "GET") return this.getCursors(url);
    if (parts[0] === "cursors" && m === "POST") return this.putCursors(req, p);
    if (parts[0] === "config" && parts.length === 1 && m === "GET") return this.getConfig();
    if (parts[0] === "config" && parts.length === 2 && m === "PUT") return this.putConfig(req, p, parts[1]);
    if (parts[0] === "media" && parts.length === 1 && m === "GET") return this.listMedia(url);
    if (parts[0] === "media" && parts.length === 2 && m === "GET") return this.getMedia(parts[1]);
    if (parts[0] === "media" && parts.length === 2 && m === "PUT") return this.putMedia(req, parts[1]);
    if (parts[0] === "tokens") return this.tokens(req, p, parts, project);
    return error(404, "not_found", `no route ${m} ${url.pathname}`);
  }

  /// Admin only: creates the project and its first owner token, bound to the
  /// creating machine's writer.
  async create(req, p, project) {
    if (!p.admin) throw new HttpError(403, "forbidden", "only the admin secret creates projects");
    const body = await readJson(req, 4096);
    const person = String(body.person || "").trim();
    const writer = String(body.writer || "").trim();
    if (!person) throw new HttpError(400, "bad_person", "person is required");
    if (!WRITER_RE.test(writer)) throw new HttpError(400, "bad_writer", `invalid writer ${JSON.stringify(writer)}`);
    if (this.meta("name")) throw new HttpError(409, "exists", `project ${project} already exists on this server`);
    const spec = this.insertToken({ person, role: "owner", epic: null, writer, expires: null, createdBy: "admin" });
    this.setMeta("name", project);
    this.setMeta("created", new Date().toISOString());
    const t = await this.storeToken(spec, project);
    this.bump();
    return json({ project, ...t }, 201);
  }

  websocket(req) {
    if ((req.headers.get("upgrade") || "").toLowerCase() !== "websocket") {
      throw new HttpError(426, "upgrade_required", "this route is a WebSocket");
    }
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    // Hibernatable: the object can be evicted while the socket stays open, and
    // no duration is billed while it is idle.
    this.ctx.acceptWebSocket(server);
    server.send(JSON.stringify({ type: "frontier", ...this.frontier() }));
    return new Response(null, { status: 101, webSocket: client });
  }

  webSocketMessage(ws, message) {
    // Clients only listen; "ping" gets the frontier back (a way to check the
    // connection without waiting for a change).
    if (message === "ping") ws.send(JSON.stringify({ type: "frontier", ...this.frontier() }));
  }

  webSocketClose(ws, code) {
    try {
      ws.close(code === 1005 ? 1000 : code, "bye");
    } catch {
      // Already closed.
    }
  }

  /// Appends lines to one writer's file. They must continue the server's
  /// chain; the first one that does not stops the batch and is refused with
  /// the server's tail. What the server already has is skipped.
  async push(req, p) {
    const body = await readJson(req, MAX_PUSH_BODY);
    const writer = String(body.writer || "");
    const lines = body.lines;
    if (!WRITER_RE.test(writer)) throw new HttpError(400, "bad_writer", `invalid writer ${JSON.stringify(writer)}`);
    if (!Array.isArray(lines) || lines.length > MAX_PUSH_LINES) {
      throw new HttpError(400, "bad_lines", `lines must be an array of at most ${MAX_PUSH_LINES} strings`);
    }
    if (!p.admin && p.writer !== writer) {
      return json(
        {
          accepted: 0, duplicates: 0, tail: this.tail(writer),
          refused: { index: 0, n: null, reason: "writer", message: `this token writes as ${p.writer}, not ${writer}` },
        },
        403,
      );
    }
    let tail = this.tail(writer);
    let accepted = 0;
    let duplicates = 0;
    let refused = null;
    const refuse = (index, n, reason, message) => {
      refused = { index, n, reason, message };
    };
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      if (typeof line !== "string" || line.length === 0 || /[\r\n]/.test(line)) {
        refuse(i, null, "invalid", "a line is a non-empty JSON object on one line");
        break;
      }
      const bytes = enc.encode(line);
      if (bytes.length > MAX_LINE_BYTES) {
        refuse(i, null, "too_large", `the line is ${bytes.length} bytes; the most is ${MAX_LINE_BYTES}`);
        break;
      }
      let e;
      try {
        e = JSON.parse(line);
      } catch {
        refuse(i, null, "invalid", "the line is not JSON");
        break;
      }
      if (!e || typeof e !== "object" || Array.isArray(e)) {
        refuse(i, null, "invalid", "the line is not a JSON object");
        break;
      }
      const n = e.n;
      if (e.writer !== writer) {
        refuse(i, n ?? null, "writer", `the line says writer ${JSON.stringify(e.writer)}, not ${writer}`);
        break;
      }
      if (!Number.isSafeInteger(n) || n < 1 || !HEX64_RE.test(e.hash || "") || !HEX64_RE.test(e.prev || "")) {
        refuse(i, n ?? null, "invalid", "the line has no valid n, prev and hash");
        break;
      }
      if (n <= tail.n) {
        const had = this.sql.exec("SELECT hash FROM lines WHERE writer = ? AND n = ?", writer, n).toArray();
        if (had.length && had[0].hash === e.hash) {
          duplicates++;
          continue;
        }
        refuse(i, n, "fork", `the server already has a different line ${n} for ${writer}: the chains forked`);
        break;
      }
      if (n !== tail.n + 1) {
        refuse(i, n, "gap", `the server's last line for ${writer} is ${tail.n}; send from ${tail.n + 1}`);
        break;
      }
      if (e.prev !== tail.hash) {
        refuse(i, n, "fork", `line ${n} does not follow the server's line ${tail.n} for ${writer}: the chains forked`);
        break;
      }
      if (!p.admin) {
        const why = actorAllowed(p, e);
        if (why) {
          refuse(i, n, "identity", why);
          break;
        }
      }
      this.storeLine(writer, n, e.hash, line, bytes);
      tail = { n, hash: e.hash };
      accepted++;
    }
    if (accepted > 0) {
      this.sql.exec(
        "INSERT INTO tails(writer, n, hash) VALUES(?, ?, ?) ON CONFLICT(writer) DO UPDATE SET n = excluded.n, hash = excluded.hash",
        writer, tail.n, tail.hash,
      );
      this.bump();
      this.notify();
    }
    const status = refused ? (refused.reason === "identity" || refused.reason === "writer" ? 403 : 409) : 200;
    return json({ accepted, duplicates, tail, refused }, status);
  }

  /// What the server has beyond `since` ({writer: n}), writer by writer, in
  /// order, up to a budget; `more` says there is more to ask for.
  async pull(req) {
    const body = await readJson(req, 1024 * 1024);
    const since = body.since && typeof body.since === "object" ? body.since : {};
    const limit = Math.min(Number(body.limit) || PULL_MAX_LINES, PULL_MAX_LINES);
    const lines = [];
    let bytes = 0;
    let more = false;
    const tails = this.tails();
    outer: for (const writer of Object.keys(tails)) {
      const from = Number(since[writer]) || 0;
      if (tails[writer].n <= from) continue;
      const rows = this.sql
        .exec("SELECT n, line FROM lines WHERE writer = ? AND n > ? ORDER BY n LIMIT ?", writer, from, limit - lines.length + 1)
        .toArray();
      for (const r of rows) {
        const line = r.line === "" ? this.chunkedLine(writer, r.n) : r.line;
        const size = line.length;
        if (lines.length >= limit || (lines.length > 0 && bytes + size > PULL_MAX_BYTES)) {
          more = true;
          break outer;
        }
        lines.push({ writer, n: r.n, line });
        bytes += size;
      }
    }
    return json({ lines, more, seq: this.seq(), tails });
  }

  getCursors(url) {
    const since = Number(url.searchParams.get("since")) || 0;
    const rows = this.sql
      .exec("SELECT reader, writer, body, stamp, seq FROM cursors WHERE seq > ? ORDER BY seq", since)
      .toArray();
    return json({ cursors: rows, seq: this.seq() });
  }

  /// Cursor files: `cursors/<reader>/<writer>.json`. Each one is written only
  /// by its own writer, last write wins by its stamp (the file's time in ms).
  async putCursors(req, p) {
    const body = await readJson(req, 4 * 1024 * 1024);
    const list = Array.isArray(body.cursors) ? body.cursors : [];
    let stored = 0;
    let skipped = 0;
    let seq = null;
    for (const c of list) {
      const reader = String(c.reader || "");
      const writer = String(c.writer || "");
      const text = String(c.body ?? "");
      const stamp = Number(c.stamp) || 0;
      if (!READER_RE.test(reader) || !WRITER_RE.test(writer)) throw new HttpError(400, "bad_cursor", "invalid reader or writer");
      if (text.length > MAX_CURSOR_BYTES) throw new HttpError(413, "too_large", "cursor file too large");
      if (!p.admin && writer !== p.writer) {
        throw new HttpError(403, "forbidden", `this token writes the cursors of ${p.writer}, not ${writer}`);
      }
      const had = this.sql.exec("SELECT body, stamp FROM cursors WHERE reader = ? AND writer = ?", reader, writer).toArray();
      if (had.length && (had[0].stamp > stamp || had[0].body === text)) {
        skipped++;
        continue;
      }
      if (seq === null) seq = this.bump();
      this.sql.exec(
        "INSERT INTO cursors(reader, writer, body, stamp, seq) VALUES(?,?,?,?,?) ON CONFLICT(reader, writer) DO UPDATE SET body = excluded.body, stamp = excluded.stamp, seq = excluded.seq",
        reader, writer, text, stamp, seq,
      );
      stored++;
    }
    if (stored) this.notify();
    return json({ stored, skipped, seq: this.seq() });
  }

  getConfig() {
    const files = {};
    for (const r of this.sql.exec("SELECT * FROM config ORDER BY name").toArray()) {
      files[r.name] = { version: r.version, sha256: r.sha256, body: r.body, by: r.by, at: r.at };
    }
    return json({ files });
  }

  /// A new version of a config file, only from an owner (or the admin), and
  /// only on top of the version the client last saw (`base`, 0 for none).
  async putConfig(req, p, name) {
    if (!CONFIG_FILES.includes(name)) throw new HttpError(404, "not_found", `${name} is not a config file the server keeps`);
    if (!p.admin && p.role !== "owner") {
      throw new HttpError(403, "forbidden", `only an owner token writes ${name} (this one is ${p.role})`);
    }
    const body = await readJson(req, MAX_CONFIG_BYTES * 2);
    const text = String(body.body ?? "");
    if (text.length > MAX_CONFIG_BYTES) throw new HttpError(413, "too_large", `${name} is too large`);
    const base = Number(body.base) || 0;
    const sha = await sha256Hex(text);
    const cur = this.sql.exec("SELECT version, sha256 FROM config WHERE name = ?", name).toArray();
    const version = cur.length ? cur[0].version : 0;
    if (cur.length && cur[0].sha256 === sha) return json({ name, version, sha256: sha, changed: false });
    if (base !== version) {
      return json(
        { error: "conflict", message: `${name} is at version ${version} on the server, not ${base}`, version, sha256: cur.length ? cur[0].sha256 : null },
        409,
      );
    }
    const next = version + 1;
    this.sql.exec(
      "INSERT INTO config(name, version, sha256, body, by, at) VALUES(?,?,?,?,?,?) ON CONFLICT(name) DO UPDATE SET version = excluded.version, sha256 = excluded.sha256, body = excluded.body, by = excluded.by, at = excluded.at",
      name, next, sha, text, p.admin ? "admin" : `${p.person} (${p.writer})`, new Date().toISOString(),
    );
    this.bump();
    this.notify();
    return json({ name, version: next, sha256: sha, changed: true });
  }

  listMedia(url) {
    const since = Number(url.searchParams.get("since")) || 0;
    const rows = this.sql.exec("SELECT name, sha256, bytes, seq FROM media WHERE seq > ? ORDER BY seq", since).toArray();
    return json({ media: rows, seq: this.seq() });
  }

  getMedia(name) {
    if (!MEDIA_RE.test(name)) throw new HttpError(400, "bad_name", "a media name is <sha256>.<ext>");
    const meta = this.sql.exec("SELECT bytes, chunks FROM media WHERE name = ?", name).toArray();
    if (!meta.length) throw new HttpError(404, "not_found", `${name} is not on the server`);
    const out = new Uint8Array(meta[0].bytes);
    let off = 0;
    for (const r of this.sql.exec("SELECT data FROM media_chunks WHERE name = ? ORDER BY i", name).toArray()) {
      const chunk = new Uint8Array(r.data);
      out.set(chunk, off);
      off += chunk.length;
    }
    return new Response(out, { headers: { "content-type": "application/octet-stream" } });
  }

  /// Stores a file by its content: the name carries its sha256, which is
  /// checked. Uploading one the server has is a no-op.
  async putMedia(req, name) {
    const m = MEDIA_RE.exec(name);
    if (!m) throw new HttpError(400, "bad_name", "a media name is <sha256>.<ext>");
    const had = this.sql.exec("SELECT bytes FROM media WHERE name = ?", name).toArray();
    if (had.length) return json({ name, stored: false });
    const buf = new Uint8Array(await req.arrayBuffer());
    if (buf.length > MAX_MEDIA_BYTES) throw new HttpError(413, "too_large", `media over ${MAX_MEDIA_BYTES} bytes`);
    const sha = await sha256Hex(buf);
    if (sha !== m[1]) throw new HttpError(400, "bad_hash", `the content's sha256 is ${sha}, not the one in the name`);
    const chunks = Math.max(1, Math.ceil(buf.length / MEDIA_CHUNK));
    for (let i = 0; i < chunks; i++) {
      const part = buf.slice(i * MEDIA_CHUNK, Math.min(buf.length, (i + 1) * MEDIA_CHUNK));
      this.sql.exec("INSERT OR REPLACE INTO media_chunks(name, i, data) VALUES(?, ?, ?)", name, i, part.buffer);
    }
    const seq = this.bump();
    this.sql.exec("INSERT INTO media(name, sha256, bytes, chunks, seq) VALUES(?,?,?,?,?)", name, sha, buf.length, chunks, seq);
    this.notify();
    return json({ name, stored: true }, 201);
  }

  /// Tokens, for an owner token or the admin:
  /// - `POST tokens` {person, role, writer, epic?, actors?, ttl_ms?}: a new one.
  ///   A coordinator needs its epic and the actor names it coordinates as.
  /// - `GET tokens`: all of them, without their hashes.
  /// - `POST tokens/<id>/revoke`.
  async tokens(req, p, parts, project) {
    if (!p.admin && p.role !== "owner") throw new HttpError(403, "forbidden", "only an owner manages tokens");
    const m = req.method;
    if (parts.length === 1 && m === "GET") {
      const rows = this.sql.exec("SELECT * FROM tokens ORDER BY created, id").toArray();
      return json({ tokens: rows.map(tokenRow) });
    }
    if (parts.length === 1 && m === "POST") {
      const body = await readJson(req, 4096);
      const role = String(body.role || "");
      const person = String(body.person || "").trim();
      const writer = String(body.writer || "").trim();
      const epic = body.epic == null ? null : String(body.epic).trim() || null;
      const actors = Array.isArray(body.actors) ? body.actors.map((a) => String(a).trim()).filter(Boolean) : [];
      const ttl = body.ttl_ms == null ? null : Number(body.ttl_ms);
      if (!ROLES.includes(role)) throw new HttpError(400, "bad_role", `role is one of ${ROLES.join(", ")}`);
      if (!person || person.length > 128) throw new HttpError(400, "bad_person", "person is required (at most 128 characters)");
      if (!WRITER_RE.test(writer)) throw new HttpError(400, "bad_writer", `invalid writer ${JSON.stringify(writer)}`);
      if (role === "coordinator" && (!epic || !actors.length)) {
        throw new HttpError(400, "bad_epic", "a coordinator token needs its epic and the names it coordinates as");
      }
      if (actors.length > 8 || actors.some((a) => a.length > 128)) throw new HttpError(400, "bad_actors", "at most 8 names");
      if (ttl != null && !(Number.isFinite(ttl) && ttl > 0)) throw new HttpError(400, "bad_ttl", "ttl_ms must be a positive number");
      const spec = this.insertToken({
        person, role, epic, actors: role === "coordinator" ? actors : [], writer,
        expires: ttl ? Date.now() + Math.floor(ttl) : null, createdBy: p.admin ? "admin" : p.id,
      });
      const t = await this.storeToken(spec, project);
      return json({ project, ...t }, 201);
    }
    if (parts.length === 3 && parts[2] === "revoke" && m === "POST") {
      await drain(req);
      const rows = this.sql.exec("SELECT * FROM tokens WHERE id = ?", parts[1]).toArray();
      if (!rows.length) throw new HttpError(404, "no_token", `there is no token ${parts[1]}`);
      if (!rows[0].revoked) {
        this.sql.exec("UPDATE tokens SET revoked = ? WHERE id = ?", new Date().toISOString(), parts[1]);
      }
      const t = this.sql.exec("SELECT * FROM tokens WHERE id = ?", parts[1]).toArray()[0];
      return json({ token: tokenRow(t) });
    }
    throw new HttpError(404, "not_found", "no such token route");
  }
}
