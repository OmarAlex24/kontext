// The protocol of server/worker.js against workerd (Miniflare): `npm test`.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { Miniflare } from "miniflare";
import { options } from "../dev.mjs";

const ADMIN = "test-admin-secret";
let mf;
let n = 0;

before(async () => {
  mf = new Miniflare(options({ adminSecret: ADMIN }));
  await mf.ready;
});
after(async () => {
  await mf.dispose();
});

const GENESIS = "0".repeat(64);
const sha = (s) => createHash("sha256").update(s).digest("hex");

async function call(method, path, { token, body, raw, headers = {} } = {}) {
  const h = { ...headers };
  if (token) h.authorization = `Bearer ${token}`;
  let b;
  if (raw !== undefined) b = raw;
  else if (body !== undefined) {
    b = JSON.stringify(body);
    h["content-type"] = "application/json";
  }
  const res = await mf.dispatchFetch(`http://localhost${path}`, { method, headers: h, body: b });
  const type = res.headers.get("content-type") || "";
  const data = type.includes("json") ? await res.json() : new Uint8Array(await res.arrayBuffer());
  return { status: res.status, data };
}

/// A fresh project with its owner token (bound to writer `omar@mac`).
async function project(writer = "omar@mac") {
  const name = `p${++n}`;
  const r = await call("POST", `/v1/p/${name}/create`, { token: ADMIN, body: { person: "omar", writer } });
  assert.equal(r.status, 201, JSON.stringify(r.data));
  return { name, token: r.data.token, writer };
}

/// A chain of ledger lines for a writer, like the binary writes them.
function chain(writer, count, { from = 1, prev = GENESIS, actor = { type: "human", name: "omar" }, tag = "" } = {}) {
  const out = [];
  for (let i = 0; i < count; i++) {
    const nn = from + i;
    const body = { v: 1, id: `J-${sha(writer + nn + tag).slice(0, 6)}`, writer, n: nn, prev, hlc: "2026-11-13T10:00:00.000Z.0000", actor, kind: "journal", title: `entry ${nn}${tag}` };
    const hash = sha(JSON.stringify(body));
    const line = JSON.stringify({ ...body, hash });
    out.push({ line, hash });
    prev = hash;
  }
  return out;
}

test("info and auth", async () => {
  const info = await call("GET", "/v1/info");
  assert.equal(info.data.kontext_server, 1);
  const p = await project();
  assert.equal((await call("GET", `/v1/p/${p.name}/frontier`)).status, 401);
  assert.equal((await call("GET", `/v1/p/${p.name}/frontier`, { token: "kxt1.bad.bad" })).status, 401);
  const who = await call("GET", `/v1/p/${p.name}/whoami`, { token: p.token });
  assert.deepEqual([who.data.role, who.data.person, who.data.writer], ["owner", "omar", "omar@mac"]);
  // The token carries the project and the writer.
  const head = JSON.parse(Buffer.from(p.token.split(".")[1], "base64url").toString());
  assert.deepEqual(head, { p: p.name, w: "omar@mac" });
  // Only the admin secret creates projects, and only once.
  assert.equal((await call("POST", `/v1/p/${p.name}/create`, { token: p.token, body: { person: "x", writer: "x@y" } })).status, 403);
  assert.equal((await call("POST", `/v1/p/${p.name}/create`, { token: ADMIN, body: { person: "x", writer: "x@y" } })).status, 409);
  // A project that does not exist.
  assert.equal((await call("GET", `/v1/p/nope/frontier`, { token: ADMIN })).status, 404);
});

test("append continues the chain; a re-push is a no-op; a fork and a gap are refused with the tail", async () => {
  const p = await project();
  const c = chain(p.writer, 5);
  let r = await call("POST", `/v1/p/${p.name}/push`, { token: p.token, body: { writer: p.writer, lines: c.slice(0, 3).map((x) => x.line) } });
  assert.equal(r.status, 200);
  assert.deepEqual([r.data.accepted, r.data.duplicates, r.data.tail.n], [3, 0, 3]);
  // Retry of the same lines plus new ones: the old ones are skipped.
  r = await call("POST", `/v1/p/${p.name}/push`, { token: p.token, body: { writer: p.writer, lines: c.map((x) => x.line) } });
  assert.deepEqual([r.data.accepted, r.data.duplicates, r.data.tail.n, r.data.refused], [2, 3, 5, null]);
  // A different line 4 (another copy of the writer wrote its own): refused.
  const fork = chain(p.writer, 2, { from: 4, prev: c[2].hash, tag: "-other" });
  r = await call("POST", `/v1/p/${p.name}/push`, { token: p.token, body: { writer: p.writer, lines: fork.map((x) => x.line) } });
  assert.equal(r.status, 409);
  assert.equal(r.data.refused.reason, "fork");
  assert.deepEqual(r.data.tail, { n: 5, hash: c[4].hash });
  // Line 6 that does not follow line 5.
  const bad = chain(p.writer, 1, { from: 6, prev: fork[1].hash });
  r = await call("POST", `/v1/p/${p.name}/push`, { token: p.token, body: { writer: p.writer, lines: [bad[0].line] } });
  assert.equal(r.data.refused.reason, "fork");
  // A gap.
  const gap = chain(p.writer, 1, { from: 9, prev: c[4].hash });
  r = await call("POST", `/v1/p/${p.name}/push`, { token: p.token, body: { writer: p.writer, lines: [gap[0].line] } });
  assert.equal(r.data.refused.reason, "gap");
  // Nothing of that was stored.
  const pull = await call("POST", `/v1/p/${p.name}/pull`, { token: p.token, body: { since: {} } });
  assert.deepEqual(pull.data.lines.map((l) => l.line), c.map((x) => x.line));
});

test("a token writes only its own writer's file", async () => {
  const p = await project();
  const other = chain("ana@laptop", 1);
  const r = await call("POST", `/v1/p/${p.name}/push`, { token: p.token, body: { writer: "ana@laptop", lines: [other[0].line] } });
  assert.equal(r.status, 403);
  assert.equal(r.data.refused.reason, "writer");
  // A line claiming another writer inside the token's own file.
  const r2 = await call("POST", `/v1/p/${p.name}/push`, { token: p.token, body: { writer: p.writer, lines: [other[0].line] } });
  assert.equal(r2.data.refused.reason, "writer");
  // The admin may import any writer's history (`kontext server connect`).
  const r3 = await call("POST", `/v1/p/${p.name}/push`, { token: ADMIN, body: { writer: "ana@laptop", lines: [other[0].line] } });
  assert.equal(r3.data.accepted, 1);
});

test("lines come back byte for byte, writer by writer, paged", async () => {
  const p = await project();
  // Characters JSON.stringify would write differently if the server re-encoded.
  const odd = { v: 1, writer: p.writer, n: 1, prev: GENESIS, title: "ñ   é 😀 \"q\" \\ </script>", hash: sha("odd") };
  const line = '{"actor":{"name":"omar","type":"human"},"hash":"' + odd.hash + '","n":1,"prev":"' + GENESIS + '","title":"ñ   é 😀 \\"q\\" \\\\ </script>","v":1,"writer":"' + p.writer + '"}';
  let r = await call("POST", `/v1/p/${p.name}/push`, { token: p.token, body: { writer: p.writer, lines: [line] } });
  assert.equal(r.data.accepted, 1, JSON.stringify(r.data));
  await call("POST", `/v1/p/${p.name}/push`, { token: ADMIN, body: { writer: "ana@laptop", lines: chain("ana@laptop", 3).map((x) => x.line) } });
  r = await call("POST", `/v1/p/${p.name}/pull`, { token: p.token, body: { since: {}, limit: 2 } });
  assert.equal(r.data.more, true);
  assert.deepEqual(r.data.lines.map((l) => [l.writer, l.n]), [["ana@laptop", 1], ["ana@laptop", 2]]);
  r = await call("POST", `/v1/p/${p.name}/pull`, { token: p.token, body: { since: { "ana@laptop": 2 } } });
  assert.deepEqual(r.data.lines.map((l) => [l.writer, l.n]), [["ana@laptop", 3], [p.writer, 1]]);
  assert.equal(r.data.lines[1].line, line);
  assert.equal(r.data.more, false);
});

test("cursors: each writer's own file, last write wins by stamp", async () => {
  const p = await project();
  const put = (body, token = p.token) => call("POST", `/v1/p/${p.name}/cursors`, { token, body: { cursors: [body] } });
  let r = await put({ reader: "omar", writer: p.writer, body: '{"omar@mac":3}', stamp: 100 });
  assert.equal(r.data.stored, 1);
  r = await put({ reader: "omar", writer: p.writer, body: '{"omar@mac":2}', stamp: 50 });
  assert.equal(r.data.skipped, 1);
  r = await put({ reader: "epic:payments", writer: p.writer, body: '{"omar@mac":5}', stamp: 200 });
  assert.equal(r.data.stored, 1);
  assert.equal((await put({ reader: "omar", writer: "ana@laptop", body: "{}", stamp: 1 })).status, 403);
  const all = await call("GET", `/v1/p/${p.name}/cursors?since=0`, { token: p.token });
  assert.deepEqual(all.data.cursors.map((c) => [c.reader, c.body]), [["omar", '{"omar@mac":3}'], ["epic:payments", '{"omar@mac":5}']]);
  const later = await call("GET", `/v1/p/${p.name}/cursors?since=${all.data.cursors[0].seq}`, { token: p.token });
  assert.deepEqual(later.data.cursors.map((c) => c.reader), ["epic:payments"]);
});

test("config: versioned, owner only, on top of the version seen", async () => {
  const p = await project();
  let r = await call("PUT", `/v1/p/${p.name}/config/project.toml`, { token: p.token, body: { base: 0, body: 'name = "x"\n' } });
  assert.deepEqual([r.status, r.data.version], [200, 1]);
  r = await call("PUT", `/v1/p/${p.name}/config/project.toml`, { token: p.token, body: { base: 0, body: 'name = "y"\n' } });
  assert.deepEqual([r.status, r.data.version], [409, 1]);
  r = await call("PUT", `/v1/p/${p.name}/config/project.toml`, { token: p.token, body: { base: 1, body: 'name = "y"\n' } });
  assert.equal(r.data.version, 2);
  assert.equal((await call("PUT", `/v1/p/${p.name}/config/other.toml`, { token: p.token, body: { base: 0, body: "" } })).status, 404);
  const g = await call("GET", `/v1/p/${p.name}/config`, { token: p.token });
  assert.equal(g.data.files["project.toml"].body, 'name = "y"\n');
  const f = await call("GET", `/v1/p/${p.name}/frontier`, { token: p.token });
  assert.equal(f.data.config["project.toml"].version, 2);
});

test("media: by sha256, chunked, checked", async () => {
  const p = await project();
  const big = new Uint8Array(2.5 * 1024 * 1024);
  for (let i = 0; i < big.length; i++) big[i] = (i * 31 + 7) & 0xff;
  const name = `${sha(big)}.png`;
  let r = await call("PUT", `/v1/p/${p.name}/media/${name}`, { token: p.token, raw: big });
  assert.deepEqual([r.status, r.data.stored], [201, true]);
  r = await call("PUT", `/v1/p/${p.name}/media/${name}`, { token: p.token, raw: big });
  assert.equal(r.data.stored, false);
  r = await call("PUT", `/v1/p/${p.name}/media/${sha("x")}.png`, { token: p.token, raw: big });
  assert.equal(r.data.error, "bad_hash");
  r = await call("GET", `/v1/p/${p.name}/media/${name}`, { token: p.token });
  assert.equal(Buffer.compare(Buffer.from(r.data), Buffer.from(big)), 0);
  const list = await call("GET", `/v1/p/${p.name}/media?since=0`, { token: p.token });
  assert.deepEqual(list.data.media.map((m) => [m.name, m.bytes]), [[name, big.length]]);
});

test("the WebSocket announces the frontier, not the lines", async () => {
  const p = await project();
  const res = await mf.dispatchFetch(`http://localhost/v1/p/${p.name}/ws`, {
    headers: { upgrade: "websocket", authorization: `Bearer ${p.token}` },
  });
  assert.equal(res.status, 101);
  const ws = res.webSocket;
  const got = [];
  let wake;
  ws.addEventListener("message", (ev) => {
    got.push(JSON.parse(ev.data));
    if (wake) wake();
  });
  ws.accept();
  const next = async (count) => {
    while (got.length < count) await new Promise((r) => (wake = r));
    return got[count - 1];
  };
  const hello = await next(1);
  assert.equal(hello.type, "frontier");
  await call("POST", `/v1/p/${p.name}/push`, { token: p.token, body: { writer: p.writer, lines: chain(p.writer, 2).map((x) => x.line) } });
  const msg = await next(2);
  assert.equal(msg.tails[p.writer].n, 2);
  assert.equal(msg.lines, undefined);
  ws.close();
  // Without a token, no socket.
  const bad = await mf.dispatchFetch(`http://localhost/v1/p/${p.name}/ws`, { headers: { upgrade: "websocket" } });
  assert.equal(bad.status, 401);
});
