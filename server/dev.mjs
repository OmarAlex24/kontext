// A local kontext server: server/worker.js in workerd, through Miniflare.
// For the tests (`cargo test` starts it) and for trying kontext by hand; never
// for production.
//
//   node server/dev.mjs [--port 8787] [--persist <dir>] [--admin-secret <s>]
//
// It prints the URL on the first line of stdout and runs until stdin closes or
// it gets SIGTERM/SIGINT. Without --persist, everything is in memory.
import { Miniflare } from "miniflare";
import { fileURLToPath } from "node:url";
import path from "node:path";

export const COMPATIBILITY_DATE = "2026-04-07";

const here = path.dirname(fileURLToPath(import.meta.url));

export function options({ port = 0, persist = null, adminSecret = "dev-admin-secret" } = {}) {
  return {
    modules: true,
    scriptPath: path.join(here, "worker.js"),
    compatibilityDate: COMPATIBILITY_DATE,
    durableObjects: { PROJECT: { className: "Project", useSQLite: true } },
    bindings: { ADMIN_SECRET: adminSecret },
    host: "127.0.0.1",
    port,
    ...(persist ? { durableObjectsPersist: persist } : {}),
  };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  const args = process.argv.slice(2);
  const opt = (name) => {
    const i = args.indexOf(name);
    return i >= 0 ? args[i + 1] : undefined;
  };
  const mf = new Miniflare(
    options({
      port: Number(opt("--port") || 0),
      persist: opt("--persist") || null,
      adminSecret: opt("--admin-secret") || process.env.KONTEXT_DEV_ADMIN_SECRET || "dev-admin-secret",
    }),
  );
  const url = await mf.ready;
  process.stdout.write(url.href.replace(/\/$/, "") + "\n");
  let stopping = false;
  const stop = async () => {
    if (stopping) return;
    stopping = true;
    await mf.dispose().catch(() => {});
    process.exit(0);
  };
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
  process.stdin.on("end", stop);
  process.stdin.on("close", stop);
  process.stdin.resume();
}
