import { hostCallError } from "../src/util.ts";

/* The engine under Deno with no browser, undeclared names fail and missing Web APIs name themselves. */
const BASE = Deno.env.get("EDGE_CDN_BASE")?.replace(/\/$/, "");
if (!BASE) throw new Error("set EDGE_CDN_BASE (npm run cdn:local in infra)");
const WASM = `${BASE}/compiler.wasm`;

// A fresh engine per test, the query string keeps the module state apart.
async function boot(name, builtins, extra = {}) {
    const engine = await import(new URL(`../src/worker/engine.ts?deno=${name}`, import.meta.url).href);
    const handlers = {};
    const labels = {};
    const pushEvent = (m) => engine.pushEvent(m);
    engine.setLoadSystemDelegate(async (url, label) => {
        const source = await import(url);
        const factory = source.default ?? source[label];
        const h = typeof factory === "function" ? factory({ pushEvent }) : factory;
        labels[url] = label;
        for (const [k, v] of Object.entries(h)) handlers[`${url}:${k}`] = v;
        return Object.keys(h);
    });
    engine.setHostCallDelegate(async (url, fn, args) => {
        const h = handlers[`${url}:${fn}`];
        if (!h) throw new Error(`no main-thread handler for '${labels[url]}.${fn}'`);
        try {
            return await h(...args);
        } catch (e) {
            throw new Error(hostCallError(labels[url], e));
        }
    });
    // Each builtin is declared the way edge.json would, by the url of its JavaScript module.
    const imports = { ...Object.fromEntries(builtins.map((b) => [b, new URL(`../builtins/${b}/src/index.js`, import.meta.url).href])), ...extra };
    await engine.load({ wasmUrl: WASM, integrity: false, imports });
    return engine;
}

// No manifest lives here, so bare names stay undeclared.
const baseUrl = new URL("./nomanifest/", import.meta.url).href;

// A packed package runs its entry and imports its own files from inside itself, the way a published one arrives.
Deno.test("deno: a packed package imports from inside itself", async () => {
    const enc = new TextEncoder();
    const framed = (bytes) => [...enc.encode(`${bytes.length}\n`), ...bytes];
    const files = { "main.py": "from .src.hello import hello\n", "src/hello.py": "def hello(name):\n    return 'hello ' + name\n", "https://example.com/dep.edge": "" };
    const packed = [...enc.encode("EDGEPKG\x01"), ...framed(enc.encode("main.py")), ...enc.encode(`${Object.keys(files).length}\n`)];
    for (const [path, text] of Object.entries(files)) packed.push(...framed(enc.encode(path)), ...framed(enc.encode(text)));
    const dir = await Deno.makeTempDir();
    await Deno.writeFile(`${dir}/greet.edge`, new Uint8Array(packed));
    const engine = await boot("package", [], { greet: new URL(`file://${dir}/greet.edge`).href });
    const lines = [];
    const { out } = await engine.run({ src: "from greet import hello\nprint(hello('edge'))", baseUrl }, (t) => lines.push(t));
    if (out !== "" || lines.join("").trim() !== "hello edge") throw new Error(`unexpected ${JSON.stringify([out, lines])}`);
});

/* A published package declares its own dependency by version, so the host has to read the lock the package carries rather than ask a registry at run time. */
Deno.test("deno: a packed package resolves its own version through the lock it carries", async () => {
    const enc = new TextEncoder();
    const framed = (bytes) => [...enc.encode(`${bytes.length}\n`), ...bytes];
    const pack = (entry, files) => {
        const out = [...enc.encode("EDGEPKG\x01"), ...framed(enc.encode(entry)), ...enc.encode(`${Object.keys(files).length}\n`)];
        for (const [path, text] of Object.entries(files)) out.push(...framed(enc.encode(path)), ...framed(enc.encode(text)));
        return new Uint8Array(out);
    };
    const dir = await Deno.makeTempDir();
    const dep = pack("main.py", { "main.py": "def shout(word):\n    return word.upper()\n" });
    await Deno.writeFile(`${dir}/dep.edge`, dep);
    const digest = [...new Uint8Array(await crypto.subtle.digest("SHA-256", dep))].map((b) => b.toString(16).padStart(2, "0")).join("");
    const url = new URL(`file://${dir}/dep.edge`).href;

    const app = pack("main.py", {
        "main.py": "from dep import shout\n\ndef greet(name):\n    return shout('hi ' + name)\n",
        "edge.json": JSON.stringify({ imports: { dep: "0.1.0" } }),
        "edge.lock": JSON.stringify({ dep: { version: "0.1.0", url, digest: `sha256-${digest}` } }),
    });
    await Deno.writeFile(`${dir}/app.edge`, app);

    const engine = await boot("lock", [], { app: new URL(`file://${dir}/app.edge`).href });
    const lines = [];
    const { out } = await engine.run({ src: "from app import greet\nprint(greet('edge'))", baseUrl }, (t) => lines.push(t));
    if (out !== "" || lines.join("").trim() !== "HI EDGE") throw new Error(`unexpected ${JSON.stringify([out, lines])}`);
});

// A version nothing resolved names the command that resolves it, rather than blaming the fetch it would have made.
Deno.test("deno: a version no lock holds names edge lock", async () => {
    const enc = new TextEncoder();
    const framed = (bytes) => [...enc.encode(`${bytes.length}\n`), ...bytes];
    const files = { "main.py": "print(1)\n", "edge.json": JSON.stringify({ imports: { dep: "0.1.0" } }) };
    const out = [...enc.encode("EDGEPKG\x01"), ...framed(enc.encode("main.py")), ...enc.encode(`${Object.keys(files).length}\n`)];
    for (const [path, text] of Object.entries(files)) out.push(...framed(enc.encode(path)), ...framed(enc.encode(text)));
    const dir = await Deno.makeTempDir();
    await Deno.writeFile(`${dir}/app.edge`, new Uint8Array(out));

    const engine = await boot("unlocked", [], { app: new URL(`file://${dir}/app.edge`).href });
    let failed = "";
    try {
        await engine.run({ src: "import app", baseUrl });
    } catch (e) {
        failed = String(e);
    }
    if (!failed.includes("'dep' is not locked, run edge lock")) throw new Error(`unexpected ${JSON.stringify(failed)}`);
});

Deno.test("deno: an undeclared name fails at compile time", async () => {
    const engine = await boot("undeclared", []);
    const { out } = await engine.run({ src: "import json\nprint(1)", baseUrl });
    if (!out.includes("module 'json' is not provided by this host and no edge.json declares it")) throw new Error(`unexpected output ${JSON.stringify(out)}`);
});

// 010100101010 THIS AND THE NEXT TEST LOAD TIME AND DOM, RESTORE THEM ONCE EDGE-PYTHON-STD PUBLISHES THEM TO THE REGISTRY.
Deno.test.ignore("deno: a declared JavaScript module answers", async () => {
    const engine = await boot("time", ["time"]);
    const lines = [];
    const { out } = await engine.run({ src: "from time import tzname\nprint(tzname())", baseUrl }, (t) => lines.push(t));
    if (out !== "") throw new Error(`run failed ${JSON.stringify(out)}`);
    if (lines.join("").trim() === "") throw new Error("tzname printed nothing");
});

Deno.test.ignore("deno: a browser module loads and names the Web API it lacks", async () => {
    const engine = await boot("dom", ["dom"]);
    const { out } = await engine.run({ src: "import dom\ndom.body()", baseUrl });
    if (!out.includes("module 'dom' needs 'document', missing in this runtime")) throw new Error(`unexpected output ${JSON.stringify(out)}`);
});

Deno.test("deno: send() names the actor scheduler it lacks", async () => {
    const engine = await boot("send", []);
    const lines = [];
    const missing = "send() needs an actor scheduler, missing in this runtime";
    const caught = await engine.run({ src: "try:\n    send('g', 'x')\nexcept RuntimeError as e:\n    print(e)", baseUrl }, (t) => lines.push(t));
    if (caught.out !== "" || lines.join("").trim() !== missing) throw new Error(`unexpected ${JSON.stringify([caught.out, lines])}`);
    const { out } = await engine.run({ src: "send('g', 'x')", baseUrl });
    if (!out.includes(missing) || !out.includes("<input>:1:1")) throw new Error(`unexpected output ${JSON.stringify(out)}`);
});

Deno.test("deno: a leftover system section is refused", async () => {
    const dir = await Deno.makeTempDir();
    await Deno.writeTextFile(`${dir}/edge.json`, JSON.stringify({ system: { time: "./time.js" } }));
    const engine = await boot("legacy", []);
    const { out } = await engine.run({ src: "import time", baseUrl: `file://${dir}/` });
    await Deno.remove(dir, { recursive: true });
    if (!out.includes("edge.json at 'edge.json': move the system entries into imports")) throw new Error(`unexpected output ${JSON.stringify(out)}`);
});

Deno.test("deno: a manifest beside a module joins its relative targets once", async () => {
    const dir = await Deno.makeTempDir();
    await Deno.mkdir(`${dir}/pkg`);
    await Deno.writeTextFile(`${dir}/edge.json`, JSON.stringify({ imports: { pkg: "./pkg/entry.py" } }));
    await Deno.writeTextFile(`${dir}/pkg/edge.json`, JSON.stringify({ imports: { _impl: "./impl.py" } }));
    await Deno.writeTextFile(`${dir}/pkg/entry.py`, "from _impl import value\n");
    await Deno.writeTextFile(`${dir}/pkg/impl.py`, "value = 42\n");
    const engine = await boot("facade", []);
    const lines = [];
    const { out } = await engine.run({ src: "from pkg import value\nprint(value)", baseUrl: `file://${dir}/` }, (t) => lines.push(t));
    await Deno.remove(dir, { recursive: true });
    if (out !== "" || lines.join("").trim() !== "42") throw new Error(`unexpected ${JSON.stringify([out, lines])}`);
});

/* A project on disk, the edge.json a run reads its grants from beside its files. */
async function project(files) {
    const dir = await Deno.makeTempDir();
    for (const [path, text] of Object.entries(files)) {
        const at = `${dir}/${path}`;
        await Deno.mkdir(at.slice(0, at.lastIndexOf("/")), { recursive: true });
        await Deno.writeTextFile(at, text);
    }
    return new URL(`file://${dir}/`).href;
}

// What a run printed and what it failed with, from a fresh engine.
async function output(name, src, base) {
    const engine = await boot(name, []);
    const lines = [];
    const { out } = await engine.run({ src, baseUrl: base }, (t) => lines.push(t));
    return { out, text: lines.join("").trim() };
}

Deno.test("deno: a system module answers only to its grant", async () => {
    const bare = await output("ungranted", "import time", baseUrl);
    if (!bare.out.includes("'main' imports time, which edge.json does not grant it")) throw new Error(`unexpected ${JSON.stringify(bare)}`);
    const base = await project({ "edge.json": JSON.stringify({ permissions: { main: ["time:wall"] } }) });
    const src = "import time\nprint(time.now() > 10 ** 18)\ntry:\n    time.now('monotonic')\nexcept PermissionError as e:\n    print(e)";
    const granted = await output("granted", src, base);
    if (granted.out !== "" || granted.text !== "True\n'main' has no time:monotonic, edge.json grants it time:wall") throw new Error(`unexpected ${JSON.stringify(granted)}`);
});

Deno.test("deno: a grant belongs to the package it names", async () => {
    const files = {
        "clock/edge.json": JSON.stringify({ name: "clock" }),
        "clock/main.py": "import time\n\ndef now():\n    return time.now()\n",
    };
    const manifest = (permissions) => JSON.stringify({ imports: { clock: "./clock/main.py" }, permissions });
    const parent = await output("parent-only", "from clock import now\nprint(now() > 0)", await project({ ...files, "edge.json": manifest({ main: ["time:wall"] }) }));
    if (!parent.out.includes("'clock' imports time, which edge.json does not grant it")) throw new Error(`unexpected ${JSON.stringify(parent)}`);
    const child = await output("child", "from clock import now\nprint(now() > 0)", await project({ ...files, "edge.json": manifest({ clock: ["time:wall"] }) }));
    if (child.out !== "" || child.text !== "True") throw new Error(`unexpected ${JSON.stringify(child)}`);
});

// A package named main answers to its dir, so it never borrows the grant of main.
Deno.test("deno: a package named main never takes the grant of main", async () => {
    const got = await output("named-main", "from clock import now\nprint(now() > 0)", await project({
        "clock/edge.json": JSON.stringify({ name: "main" }),
        "clock/main.py": "import time\n\ndef now():\n    return time.now()\n",
        "edge.json": JSON.stringify({ imports: { clock: "./clock/main.py" }, permissions: { main: ["time:wall"] } }),
    }));
    if (!got.out.includes("'clock/' imports time, which edge.json does not grant it")) throw new Error(`unexpected ${JSON.stringify(got)}`);
});

// A module the root imports by a ./ path sits in the root's own dir, so it is the program's code.
Deno.test("deno: a helper the root imports by a ./ path belongs to main", async () => {
    const base = await project({
        "edge.json": JSON.stringify({ imports: { helper: "./helper.py" }, permissions: { main: ["time:wall"] } }),
        "helper.py": "import time\n\ndef later():\n    return time.now() > 0\n",
    });
    const got = await output("helper", "from helper import later\nprint(later())", base);
    if (got.out !== "" || got.text !== "True") throw new Error(`unexpected ${JSON.stringify(got)}`);
});

// However deep a package sits, it answers to its own grant, never to the package that imported it.
Deno.test("deno: a grandchild answers to its own grant", async () => {
    const files = {
        "clock/edge.json": JSON.stringify({ name: "clock", imports: { trace: "./trace/main.py" } }),
        "clock/main.py": "from trace import stamp\n\ndef now():\n    return stamp()\n",
        "clock/trace/edge.json": JSON.stringify({ name: "trace" }),
        "clock/trace/main.py": "import time\n\ndef stamp():\n    return time.now() > 0\n",
    };
    const manifest = (permissions) => JSON.stringify({ imports: { clock: "./clock/main.py" }, permissions });
    const src = "from clock import now\nprint(now())";
    const borrowed = await output("borrowed", src, await project({ ...files, "edge.json": manifest({ main: ["time:wall"], clock: ["time:wall"] }) }));
    if (!borrowed.out.includes("'trace' imports time, which edge.json does not grant it")) throw new Error(`unexpected ${JSON.stringify(borrowed)}`);
    const own = await output("own", src, await project({ ...files, "edge.json": manifest({ trace: ["time:wall"] }) }));
    if (own.out !== "" || own.text !== "True") throw new Error(`unexpected ${JSON.stringify(own)}`);
});

Deno.test("deno: net reaches only the hosts its package holds", async () => {
    const server = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen() {} }, (req) => new Response(`got ${new URL(req.url).pathname}`));
    const port = server.addr.port;
    const base = await project({ "edge.json": JSON.stringify({ permissions: { main: ["net:127.0.0.1"] } }) });
    const src = [
        "import net",
        `r = net.request('GET', 'http://127.0.0.1:${port}/items')`,
        "status, headers = net.response(r)",
        "print(status, net.read(r), net.read(r))",
        "try:",
        `    net.request('GET', 'http://localhost:${port}/')`,
        "except PermissionError as e:",
        "    print(e)",
    ].join("\n");
    const got = await output("net", src, base);
    await server.shutdown();
    if (got.out !== "" || got.text !== "200 b'got /items' None\n'main' has no net:localhost, edge.json grants it net:127.0.0.1") throw new Error(`unexpected ${JSON.stringify(got)}`);
});

// The pdk example, built by `cargo build --release --target wasm32-unknown-unknown -p slugify-mod`.
const PLUGIN = new URL("../../target/wasm32-unknown-unknown/release/slugify_mod.wasm", import.meta.url);

Deno.test("deno: a plugin reaches system calls and awaits the ones that wait", async () => {
    const server = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen() {} }, () => new Response("ok", { status: 201 }));
    const base = await project({ "edge.json": JSON.stringify({ imports: { slugify_mod: "./slugify_mod.wasm" }, permissions: { main: ["time:wall", "net:127.0.0.1"] } }) });
    await Deno.writeFile(new URL("slugify_mod.wasm", base), await Deno.readFile(PLUGIN));
    const src = [
        "from slugify_mod import wall_ns, status_of",
        "print(wall_ns() > 10 ** 18)",
        `print(status_of('http://127.0.0.1:${server.addr.port}/'))`,
        "try:",
        "    status_of('http://127.0.0.1:1/')",
        "except OSError as e:",
        "    print(type(e).__name__)",
        "try:",
        "    status_of('http://evil.example/')",
        "except PermissionError as e:",
        "    print(e)",
    ].join("\n");
    const got = await output("plugin-system", src, base);
    await server.shutdown();
    if (got.out !== "" || got.text !== "True\n201\nOSError\n'main' has no net:evil.example, edge.json grants it net:127.0.0.1") throw new Error(`unexpected ${JSON.stringify(got)}`);
});

Deno.test("deno: the clock stays virtual until a package holds time", async () => {
    let t0 = performance.now();
    const virtual = await output("virtual", "sleep(3600)\nprint('an hour, at once')", baseUrl);
    if (virtual.text !== "an hour, at once" || performance.now() - t0 > 2000) throw new Error(`unexpected ${JSON.stringify(virtual)}`);
    const base = await project({ "edge.json": JSON.stringify({ permissions: { main: ["time:monotonic"] } }) });
    t0 = performance.now();
    const wall = await output("wall", "sleep(0.05)\nprint('waited')", base);
    if (wall.text !== "waited" || performance.now() - t0 < 40) throw new Error(`the wall clock did not wait ${JSON.stringify(wall)}`);
});

Deno.test("deno: a malformed permissions section stops the run", async () => {
    const base = await project({ "edge.json": JSON.stringify({ permissions: { main: "time:wall" } }) });
    const engine = await boot("malformed", []);
    let message = "";
    try {
        await engine.run({ src: "print(1)", baseUrl: base });
    } catch (e) {
        message = e.message;
    }
    if (!message.includes("edge.json at 'edge.json': permissions for 'main' must be a list of entries")) throw new Error(`unexpected ${JSON.stringify(message)}`);
});
