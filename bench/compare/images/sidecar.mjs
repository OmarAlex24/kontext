// dev.mjs binds 127.0.0.1, which no other container can reach. This imports its
// options() unchanged and only overrides the host and the port; kontext's code
// is not changed (design §2.3). Runs until SIGTERM, not until stdin closes.
import { Miniflare } from "miniflare";
import { options } from "./dev.mjs";

const port = Number(process.env.KONTEXT_SIDECAR_PORT || 8787);
const adminSecret = process.env.KONTEXT_DEV_ADMIN_SECRET || "dev-admin-secret";
const mf = new Miniflare({ ...options({ port, adminSecret }), host: "0.0.0.0" });
const url = await mf.ready;
process.stdout.write(`kontext sidecar listening on ${url.href}\n`);
const stop = async () => { await mf.dispose().catch(() => {}); process.exit(0); };
process.on("SIGTERM", stop);
process.on("SIGINT", stop);
setInterval(() => {}, 1 << 30);
