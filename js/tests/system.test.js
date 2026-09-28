/* The system calls on their own, each opened for one package with the scopes the root grants it. */
import { check, scopes, unmet } from "../src/system/grants.ts";
import net from "../src/system/net.ts";
import time from "../src/system/time.ts";

const denied = (fn, message) => {
    try {
        fn();
    } catch (e) {
        if (e.name !== "PermissionError" || e.message !== message) throw new Error(`unexpected ${e.name}: ${e.message}`);
        return;
    }
    throw new Error("expected a PermissionError");
};

Deno.test("system: a package holds its own entries and those for all", () => {
    const permissions = { all: ["time:wall"], main: ["net:api.example.com", "time:zone"], http: ["net"] };
    if (JSON.stringify(scopes(permissions, "main", "time")) !== '["wall","zone"]') throw new Error("main time");
    if (JSON.stringify(scopes(permissions, "main", "net")) !== '["api.example.com"]') throw new Error("main net");
    if (JSON.stringify(scopes(permissions, "http", "net")) !== "[]") throw new Error("an entry without a scope holds the module alone");
    if (scopes(permissions, "http", "fs") !== null || scopes(permissions, "analytics", "net") !== null) throw new Error("an ungranted module");
    if (scopes(permissions, "", "time") !== null) throw new Error("code outside any package holds nothing");
});

Deno.test("system: a package asks under main and all, and only the root's grant meets it", () => {
    const root = { all: ["time:wall"], analytics: ["net:api.telemetry.com"], http: ["net:api.example.com"] };
    const asks = (pkg, section) => JSON.stringify(unmet(root, pkg, section));
    if (asks("analytics", { main: ["net:api.telemetry.com", "time:wall"] }) !== "[]") throw new Error("a met ask");
    if (asks("analytics", { main: ["time:monotonic"], all: ["net:evil.example"] }) !== '["time:monotonic","net:evil.example"]') throw new Error("unmet asks");
    if (asks("http", { main: ["net"] }) !== "[]") throw new Error("a bare ask is met by any entry for its module");
    if (asks("kv", { main: ["net"] }) !== '["net"]') throw new Error("a bare ask the root never meets");
    if (asks("analytics", { http: ["net:evil.example"] }) !== "[]") throw new Error("what a package grants others is no ask");
});

Deno.test("system: a malformed permissions section says what it needs", () => {
    if (check(undefined) !== null || check({ all: ["time:monotonic"], main: ["net:api.example.com", "net"] }) !== null) throw new Error("a valid section");
    const cases = [
        [{ main: "net:api.example.com" }, "permissions for 'main' must be a list of entries such as \"net:api.example.com\""],
        [{ main: ["fs:/tmp"] }, "permissions for 'main' name 'fs', which is not a system module (net, time)"],
        [{ main: ["time:lunar"] }, "permissions for 'main' give time the scope 'lunar', which it does not have"],
        [{ main: ["net:https://api.example.com/"] }, "permissions for 'main' give net the scope 'https://api.example.com/', which it does not have"],
        [["net"], "permissions must map each package to a list of entries"],
    ];
    for (const [section, want] of cases) {
        if (check(section) !== want) throw new Error(`${JSON.stringify(section)} gave ${check(section)}`);
    }
});

Deno.test("system: time answers only the clocks a package holds", () => {
    const clock = time("main", ["wall"]).calls;
    if (typeof clock.now() !== "bigint") throw new Error("the wall clock");
    denied(() => clock.now("monotonic"), "'main' has no time:monotonic, edge.json grants it time:wall");
    denied(() => time("analytics", []).calls.zone(), "'analytics' has no time:zone, edge.json grants it nothing");
    const [name, offset] = time("main", ["zone"]).calls.zone();
    if (typeof name !== "string" || !Number.isInteger(offset)) throw new Error("the zone");
});

Deno.test("system: a batch answers many calls of a module in one crossing", async () => {
    const clock = time("main", ["wall", "zone"]).calls;
    const [now, zone] = clock.batch([["now"], ["zone"]]);
    if (typeof now !== "bigint" || !Array.isArray(zone)) throw new Error("a batch of calls that answer at once");
    denied(() => clock.batch([["now", "monotonic"]]), "'main' has no time:monotonic, edge.json grants it time:wall, time:zone");
    for (const bad of [[["tick"]], "now", [["now"], 7], [["batch"]]]) {
        try {
            clock.batch(bad);
            throw new Error(`${JSON.stringify(bad)} was taken`);
        } catch (e) {
            if (e.name !== "ValueError") throw e;
        }
    }
    // A call that waits holds the batch until every one of them settles.
    const server = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen() {} }, () => new Response("hi", { status: 201 }));
    const at = `http://127.0.0.1:${server.addr.port}/`;
    const web = net("main", ["127.0.0.1"]);
    const [a, b] = web.calls.batch([["request", "GET", at], ["request", "GET", at]]);
    const heads = await web.calls.batch([["response", a], ["response", b]]);
    const bodies = await web.calls.batch([["read", a], ["read", b]]);
    web.close();
    await server.shutdown();
    const text = bodies.map((body) => new TextDecoder().decode(body)).join(" ");
    if (heads[0][0] !== 201 || heads[1][0] !== 201 || text !== "hi hi") throw new Error(`unexpected ${JSON.stringify(heads)} ${text}`);
});

Deno.test("system: net reaches only its hosts and the ids its package opened", async () => {
    const server = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen() {} }, () => new Response("hi"));
    const at = `http://127.0.0.1:${server.addr.port}/`;
    const main = net("main", ["127.0.0.1"]);
    const other = net("analytics", []);
    denied(() => main.calls.request("GET", "http://evil.example/"), "'main' has no net:evil.example, edge.json grants it net:127.0.0.1");
    denied(() => other.calls.connect("ws://127.0.0.1:1/"), "'analytics' has no net:127.0.0.1, edge.json grants it nothing");
    const id = main.calls.request("GET", at);
    try {
        other.calls.read(id);
        throw new Error("another package read the id");
    } catch (e) {
        if (e.name !== "ValueError") throw e;
    }
    const [status] = await main.calls.response(id);
    const body = new TextDecoder().decode(await main.calls.read(id));
    main.close();
    await server.shutdown();
    if (status !== 200 || body !== "hi") throw new Error(`unexpected ${status} ${body}`);
});
