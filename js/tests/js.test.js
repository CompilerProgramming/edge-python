// deno-lint-ignore no-import-prefix
import { chromium } from "npm:playwright@latest";
import { readFileSync } from "node:fs";
import { Buffer } from "node:buffer";

// One CDN host serves every family under a path prefix (/std, /js).
const CDN_HOST = "cdn.edgepython.com";
// The staged CDN this run tests, a tmp prefix in CI or the local one from infra.
const BASE = Deno.env.get("EDGE_CDN_BASE")?.replace(/\/$/, "");
if (!BASE) throw new Error("set EDGE_CDN_BASE (npm run cdn:local in infra)");

const REPO = new URL("../../", import.meta.url).pathname; // edge-python/ repo root
const cases = JSON.parse(readFileSync(new URL("./js.json", import.meta.url)));
const PKG = JSON.parse(readFileSync(new URL("./app/edge.json", import.meta.url)));
// Negative fixtures, only their own cases import them, abi2 and ui fail to load by design.
const FIXTURES = new Set(["trap", "abi2", "ui"]);
// star-import every project module, official names sit at CDN urls and the cases import them explicitly
const star = (imports) => Object.entries(imports).flatMap(([k, v]) => (FIXTURES.has(k) || String(v).includes("://") ? [] : `from ${k} import *`));
const PRELUDE = star(PKG.imports).join("\n") + "\n";
const TYPES = {
    ".js": "text/javascript", ".wasm": "application/wasm", ".html": "text/html",
    ".py": "text/x-python", ".json": "application/json",
};

/* The official origin answers from BASE, decoded bytes and the CDN's own headers, CORS included. */
async function cdn(route, url) {
    const res = await fetch(BASE + url.pathname + url.search);
    const headers = Object.fromEntries([...res.headers].filter(([k]) => k !== "content-encoding" && k !== "content-length"));
    return route.fulfill({ status: res.status, headers, body: Buffer.from(await res.arrayBuffer()) });
}

/* Minimal wasm-pdk module built by hand, `__edge_abi_version` reports `abi` and `boom` traps when called. */
function pdkModule(abi) {
    const enc = new TextEncoder();
    const leb = (n) => { const out = []; do { let b = n & 0x7f; n >>>= 7; if (n !== 0) b |= 0x80; out.push(b); } while (n !== 0); return out; };
    const vec = (items) => [...leb(items.length), ...items.flat()];
    const name = (s) => vec([...enc.encode(s)].map((b) => [b]));
    const section = (id, body) => [id, ...leb(body.length), ...body];
    const body = (code) => { const b = [0x00, ...code, 0x0b]; return [...leb(b.length), ...b]; };
    const types = section(1, vec([[0x60, 0x00, 0x01, 0x7f], [0x60, 0x01, 0x7f, 0x01, 0x7f], [0x60, 0x03, 0x7f, 0x7f, 0x7f, 0x01, 0x7f]]));
    const funcs = section(3, vec([[0], [1], [2]]));
    const memory = section(5, vec([[0x00, 0x01]]));
    const exports = section(7, vec([
        [...name("memory"), 0x02, 0x00],
        [...name("__edge_abi_version"), 0x00, 0x00],
        [...name("__edge_alloc"), 0x00, 0x01],
        [...name("boom"), 0x00, 0x02],
    ]));
    const code = section(10, vec([body([0x41, ...leb(abi)]), body([0x41, 0x00]), body([0x00])]));
    // Buffer, not Uint8Array, route.fulfill never delivers a plain typed array body.
    return Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, ...types, ...funcs, ...memory, ...exports, ...code]);
}

/* Boots one worker through createWorker on index.html, then feeds every js.json case to it, comparing what it printed for output cases and the run trace for error cases. */
Deno.test("js: createWorker runs the corpus in a page", async () => {
    const browser = await chromium.launch();
    const page = await browser.newPage();
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
    const requested = [];
    page.on("request", (q) => requested.push(q.url()));

    const strays = new Set(); // requests to any host but the CDN and the page fixtures, each fails the test
    await page.route("**/*", (r) => {
        const u = new URL(r.request().url());
        if (u.host === CDN_HOST) return cdn(r, u);
        if (u.host !== "localhost") {
            strays.add(`unexpected request to ${u.href}`);
            return r.abort();
        }
        if (u.pathname.endsWith("/app/trap.wasm")) return r.fulfill({ contentType: "application/wasm", body: pdkModule(1) });
        if (u.pathname.endsWith("/app/abi2.wasm")) return r.fulfill({ contentType: "application/wasm", body: pdkModule(2) });
        const ext = u.pathname.slice(u.pathname.lastIndexOf("."));
        try { return r.fulfill({ contentType: TYPES[ext] ?? "application/octet-stream", body: readFileSync(REPO + u.pathname.slice(1)) }); }
        catch { return r.fulfill({ status: 404 }); }
    });
    // A mock WebSocket echo server so network cases can open, echo and close sockets without leaving the page.
    await page.routeWebSocket("wss://localhost/echo", (ws) => {
        ws.onMessage((m) => ws.send(m));
    });
    await page.goto("http://localhost/js/tests/index.html");

    try {
        // Boot one tag without an entry, then reuse its worker for every case via run().
        // The worker reads app/edge.json itself, through the base every run passes.
        await page.evaluate(async (host) => {
            const { createWorker } = await import(host);
            globalThis.worker = await createWorker();
            globalThis.base = new URL("./app/", location.href).href;
        }, `https://${CDN_HOST}/js/src/index.js`);

        const reqd = (frag) => requested.some((u) => u.includes(frag));

        // 010100101010 A PENDING CASE WAITS ON EDGE-PYTHON-STD PUBLISHING ITS PACKAGES, SKIPPED UNTIL THEN.
        for (const c of cases.filter((c) => !c.pending)) {
            errors.length = 0;
            const got = await page.evaluate(async (src) => {
                const lines = [];
                globalThis.worker.onOutput((t) => lines.push(t));
                // A module that fails to load rejects run(), surface it as out for error cases.
                try {
                    const { out } = await globalThis.worker.run(src, { baseUrl: globalThis.base });
                    return { printed: lines.join("").trim(), out };
                } catch (e) {
                    return { printed: lines.join("").trim(), out: String((e && e.message) || e) };
                }
            }, PRELUDE + c.script);

            if (c.error) {
                if (!got.out.includes(c.error)) {
                    throw new Error(`script:\n${c.script}\n  want error containing: ${JSON.stringify(c.error)}\n  got out: ${JSON.stringify(got.out)}\n  errors: ${errors.join(" | ") || "(none)"}`);
                }
            } else if (got.printed !== c.expect) {
                throw new Error(`script:\n${c.script}\n  got:  ${JSON.stringify(got.printed)}\n  want: ${JSON.stringify(c.expect)}\n  out: ${JSON.stringify(got.out)}\n  errors: ${errors.join(" | ") || "(none)"}`);
            }
        }

        // Park on receive(), save, finish, restore, steer differently.
        const snap = await page.evaluate(async () => {
            const worker = globalThis.worker;
            const chunks = [];
            worker.onOutput((c) => chunks.push(c));
            const src = "history = []\nwhile True:\n    m = receive()\n    if m == 'stop':\n        break\n    history.append(m)\nprint('|'.join(history))";
            const running = worker.run(src);
            const parked = async () => {
                for (let i = 0; i < 100; i++) {
                    if (JSON.stringify(await worker.stateStack()).includes("waiting_event")) return;
                    await new Promise((r) => setTimeout(r, 20));
                }
                throw new Error("run never parked on receive()");
            };
            await parked();
            worker.pushEvent("a");
            await parked();
            const blob = await worker.saveState();
            const globalsAtSave = await worker.stateGlobals();
            worker.pushEvent("b");
            worker.pushEvent("stop");
            await running;
            const first = chunks.join("");
            chunks.length = 0;
            const resumed = worker.restoreState(blob);
            worker.pushEvent("c");
            worker.pushEvent("d");
            worker.pushEvent("stop");
            await resumed;
            return { first, second: chunks.join(""), globalsAtSave, blobLen: blob.length };
        });
        if (snap.first !== "a|b\n") throw new Error(`snapshot: original run produced ${JSON.stringify(snap.first)}`);
        if (snap.second !== "a|c|d\n") throw new Error(`snapshot: restored run produced ${JSON.stringify(snap.second)}`);
        if (snap.globalsAtSave.history !== "['a']") throw new Error(`snapshot: stateGlobals saw ${JSON.stringify(snap.globalsAtSave)}`);
        if (!(snap.blobLen > 100)) throw new Error(`snapshot: implausible blob length ${snap.blobLen}`);

        // A suspension-free program still pauses and snapshots.
        const pre = await page.evaluate(async () => {
            const worker = globalThis.worker;
            const chunks = [];
            worker.onOutput((c) => chunks.push(c));
            await worker.setPreemptInterval(50000);
            const src = "n = 0\nwhile n < 1000000:\n    n = n + 1\nprint('done', n)";
            const running = worker.run(src);
            await worker.pause();
            const globalsAtPause = await worker.stateGlobals();
            const blob = await worker.saveState();
            worker.resume();
            await running;
            const first = chunks.join("");
            chunks.length = 0;
            await worker.restoreState(blob);
            await worker.setPreemptInterval(0);
            return { first, second: chunks.join(""), globalsAtPause, blobLen: blob.length };
        });
        if (pre.first !== "done 1000000\n") throw new Error(`preempt: original run produced ${JSON.stringify(pre.first)}`);
        if (pre.second !== "done 1000000\n") throw new Error(`preempt: restored run produced ${JSON.stringify(pre.second)}`);
        const pausedAt = Number(pre.globalsAtPause.n);
        if (!(pausedAt > 0 && pausedAt < 1000000)) throw new Error(`preempt: expected a mid-loop pause, n was ${JSON.stringify(pre.globalsAtPause.n)}`);
        if (!(pre.blobLen > 100)) throw new Error(`preempt: implausible blob length ${pre.blobLen}`);

        // A pause on an event yield holds the program until resume().
        const evPause = await page.evaluate(async () => {
            const worker = globalThis.worker;
            const chunks = [];
            worker.onOutput((c) => chunks.push(c));
            const running = worker.run("m = receive()\nn = receive()\nprint(m, n)");
            let sawPark = false;
            for (let i = 0; i < 100; i++) {
                if (JSON.stringify(await worker.stateStack()).includes("waiting_event")) { sawPark = true; break; }
                await new Promise((r) => setTimeout(r, 20));
            }
            if (!sawPark) throw new Error("run never parked on receive()");
            const parked = worker.pause();
            worker.pushEvent("a");
            if (!await parked) throw new Error("pause() did not park an event-parked run");
            worker.pushEvent("b");
            await new Promise((r) => setTimeout(r, 200));
            const held = chunks.join("");
            worker.resume();
            await running;
            return { held, out: chunks.join("") };
        });
        if (evPause.held !== "") throw new Error(`pause: event-parked run kept running after pause(), saw ${JSON.stringify(evPause.held)}`);
        if (evPause.out !== "a b\n") throw new Error(`pause: after resume expected 'a b\\n', got ${JSON.stringify(evPause.out)}`);

        // 010100101010 THIS BLOCK SLEEPS THROUGH THE TIME PACKAGE, RESTORE IT ONCE EDGE-PYTHON-STD PUBLISHES TIME TO THE REGISTRY.
        // A pause requested during a sleep parks the run once the timer fires.
        // const tmPause = await page.evaluate(async () => {
        //     const worker = globalThis.worker;
        //     const chunks = [];
        //     worker.onOutput((c) => chunks.push(c));
        //     const running = worker.run("import time\nprint('start')\ntime.sleep(0.5)\nprint('end')");
        //     let sawSleep = false;
        //     for (let i = 0; i < 100; i++) {
        //         if (chunks.join("").includes("start")) { sawSleep = true; break; }
        //         await new Promise((r) => setTimeout(r, 20));
        //     }
        //     if (!sawSleep) throw new Error("run never reached sleep()");
        //     const parked = await worker.pause();
        //     await new Promise((r) => setTimeout(r, 300));
        //     const held = chunks.join("");
        //     worker.resume();
        //     await running;
        //     return { parked, held, out: chunks.join("") };
        // });
        // if (tmPause.parked !== true) throw new Error("pause: sleep-parked run did not report parked");
        // if (tmPause.held !== "start\n") throw new Error(`pause: timer-parked run kept running after pause(), saw ${JSON.stringify(tmPause.held)}`);
        // if (tmPause.out !== "start\nend\n") throw new Error(`pause: after resume expected 'start\\nend\\n', got ${JSON.stringify(tmPause.out)}`);

        // A cap the embedder declares reaches the engine, the sandbox default finishes this loop.
        const capped = await page.evaluate(async (host) => {
            const { createWorker } = await import(host);
            const worker = await createWorker({ wasmUrl: "https://cdn.edgepython.com/compiler.wasm", limits: { ops: 1000 } });
            const { out } = await worker.run("n = 0\nfor i in range(100000):\n    n = n + 1\nprint(n)");
            worker.dispose();
            return out;
        }, `https://${CDN_HOST}/js/src/index.js`);
        if (!capped.includes("budget exceeded")) throw new Error(`declared op cap: expected a budget error, got ${JSON.stringify(capped)}`);

        // Laziness, only what the corpus imports gets fetched, and a JavaScript import is refused before any fetch.
        if (reqd("/app/ui.js")) throw new Error("ui is JavaScript, yet ui.js was fetched");
        // 010100101010 THE CASES THAT IMPORT JSON, TIME AND NETWORK ARE PENDING, RESTORE THESE CHECKS WITH THEM.
        // if (!reqd("json.wasm")) throw new Error("json imported but json.wasm never fetched");
        // if (!reqd("/js/builtins/time")) throw new Error("time imported but its JavaScript module never loaded");
        if (reqd("re.wasm")) throw new Error("re declared but never imported, yet re.wasm was fetched (not lazy)");
        // if (!reqd("/js/builtins/network")) throw new Error("network imported by the ws cases but never loaded");

        // The IndexedDB cache survives a versionless boot and is wiped only by a version mismatch.
        const idb = await page.evaluate(async (host) => {
            if (!globalThis.worker.integrityActive) return null;
            const readStore = () => new Promise((res, rej) => {
                const req = indexedDB.open("edgepython", 1);
                req.onsuccess = () => {
                    const db = req.result;
                    const tx = db.transaction("lockfile");
                    const store = tx.objectStore("lockfile");
                    const out = { count: 0, version: null };
                    store.count().onsuccess = (e) => { out.count = e.target.result; };
                    store.get("\0v").onsuccess = (e) => { out.version = e.target.result ?? null; };
                    tx.oncomplete = () => { db.close(); res(out); };
                    tx.onerror = () => rej(tx.error);
                };
                req.onerror = () => rej(req.error);
            });
            const { createWorker } = await import(host);
            const spawn = (opts) => createWorker({ wasmUrl: "https://cdn.edgepython.com/compiler.wasm", ...opts });
            const before = await readStore();
            const plain = await spawn();
            const afterPlain = await readStore();
            plain.dispose();
            const v1 = await spawn({ version: "t-v1" });
            const afterV1 = await readStore();
            v1.dispose();
            const v1again = await spawn({ version: "t-v1" });
            const afterV1again = await readStore();
            v1again.dispose();
            const v2 = await spawn({ version: "t-v2" });
            const afterV2 = await readStore();
            v2.dispose();
            return { before, afterPlain, afterV1, afterV1again, afterV2 };
        }, `https://${CDN_HOST}/js/src/index.js`);
        if (idb) {
            if (!(idb.before.count > 0)) throw new Error(`cache: corpus left an empty lockfile store ${JSON.stringify(idb.before)}`);
            if (idb.afterPlain.count !== idb.before.count) throw new Error(`cache: versionless boot wiped the cache ${JSON.stringify(idb)}`);
            if (idb.afterV1.count !== 1 || idb.afterV1.version !== "t-v1") throw new Error(`cache: fresh version should wipe then stamp ${JSON.stringify(idb.afterV1)}`);
            if (idb.afterV1again.count !== 1 || idb.afterV1again.version !== "t-v1") throw new Error(`cache: matching version wiped the cache ${JSON.stringify(idb.afterV1again)}`);
            if (idb.afterV2.count !== 1 || idb.afterV2.version !== "t-v2") throw new Error(`cache: version mismatch should wipe then restamp ${JSON.stringify(idb.afterV2)}`);
        }
        if (strays.size) throw new Error([...strays].join("\n"));
    } catch (e) {
        // A stray request explains any failure it caused, report it first.
        throw strays.size ? new Error([...strays].join("\n"), { cause: e }) : e;
    } finally {
        await browser.close();
    }
});

// The blob bootstrap posts a requestless error when the cross-origin import fails, createWorker must reject with it instead of hanging.
Deno.test("js: createWorker rejects on a worker bootstrap failure", async () => {
    const { createWorker } = await import(new URL("../src/index.ts", import.meta.url).href);
    const RealWorker = globalThis.Worker;
    const hadLocation = "location" in globalThis;
    const RealLocation = globalThis.location;
    class StubWorker {
        constructor() {
            this.onmessage = null;
            this.onerror = null;
        }
        postMessage(msg) {
            if (msg.type === "load") {
                queueMicrotask(() => this.onmessage?.({ data: { type: "error", message: "worker bootstrap failed: boom" } }));
            }
        }
        terminate() {}
    }
    Object.defineProperty(globalThis, "Worker", { value: StubWorker, configurable: true, writable: true });
    // A page origin different from the module origin forces the Blob bootstrap path.
    globalThis.location = new URL("http://localhost/");
    try {
        let timer;
        const result = await Promise.race([
            createWorker().then(() => null, (e) => e),
            new Promise((_, rej) => { timer = setTimeout(() => rej(new Error("createWorker hung on bootstrap failure")), 5000); }),
        ]);
        clearTimeout(timer);
        if (!(result instanceof Error)) throw new Error("createWorker resolved despite the bootstrap failure");
        if (!String(result.message).includes("worker bootstrap failed: boom")) {
            throw new Error("unexpected rejection: " + result.message);
        }
        // Let the bootstrap's deferred revokeObjectURL timer fire.
        await new Promise((r) => setTimeout(r, 10));
    } finally {
        Object.defineProperty(globalThis, "Worker", { value: RealWorker, configurable: true, writable: true });
        if (hadLocation) globalThis.location = RealLocation;
        else delete globalThis.location;
    }
});

