/* The system calls on their own, each opened for one package with the scopes the root grants it. */
import { check, scopes, unmet } from "../src/system/grants.ts";
import net from "../src/system/net.ts";
import secret from "../src/system/secret.ts";
import time from "../src/system/time.ts";

const raises = (fn, name, message) => {
    try {
        fn();
    } catch (e) {
        if (e.name !== name || (message !== undefined && e.message !== message)) throw new Error(`unexpected ${e.name}: ${e.message}`);
        return;
    }
    throw new Error(`expected a ${name}`);
};

const denied = (fn, message) => raises(fn, "PermissionError", message);

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
    const valid = { all: ["time:monotonic"], main: ["net:api.example.com", "net", "net:10.0.0.255", "net:api.example.com/v2/items", "net:a.test/", "net:a.test/v1.2/@dylan", "secret", "secret:API_KEY", "secret:_V2"] };
    if (check(undefined) !== null || check(valid) !== null) throw new Error(`a valid section, ${check(valid)}`);
    const cases = [
        [{ main: "net:api.example.com" }, "permissions for 'main' must be a list of entries such as \"net:api.example.com\""],
        [{ main: ["fs:/tmp"] }, "permissions for 'main' name 'fs', which is not a system module (net, secret, time)"],
        ...["api_key", "1KEY", "API-KEY", ""].map((name) => [{ main: [`secret:${name}`] }, `permissions for 'main' give secret the scope '${name}', which it does not have`]),
        [{ main: ["time:lunar"] }, "permissions for 'main' give time the scope 'lunar', which it does not have"],
        [{ main: ["net:https://api.example.com/"] }, "permissions for 'main' give net the scope 'https://api.example.com/', which it does not have"],
        [{ main: ["net:a.test;script-src"] }, "permissions for 'main' give net the scope 'a.test;script-src', which it does not have"],
        [{ main: ["net:user@a.test"] }, "permissions for 'main' give net the scope 'user@a.test', which it does not have"],
        [{ main: ["net:API.example.com"] }, "permissions for 'main' give net the scope 'API.example.com', which it does not have"],
        [{ main: ["net:a.test/api/.."] }, "permissions for 'main' give net the scope 'a.test/api/..', which it does not have"],
        [{ main: ["net:a.test/caf%C3%A9"] }, "permissions for 'main' give net the scope 'a.test/caf%C3%A9', which it does not have"],
        [{ main: ["net:a.test//api"] }, "permissions for 'main' give net the scope 'a.test//api', which it does not have"],
        ...["net:[::1]", "net:0177.0.0.1", "net:2130706433", "net:0x7f.1"].map((e) => [{ main: [e] }, `permissions for 'main' give net the scope '${e.slice(4)}', which it does not have`]),
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

Deno.test("system: secret reads only the names a package holds, from what the host keeps", () => {
    const kept = { API_KEY: "k-123", OTHER: "leak" };
    const asked = [];
    const host = { secret: (name) => (asked.push(name), kept[name] ?? null) };
    const { read } = secret("main", ["API_KEY", "GONE"], host).calls;
    if (read("API_KEY") !== "k-123") throw new Error("a granted name");
    denied(() => read("OTHER"), "'main' has no secret:OTHER, edge.json grants it secret:API_KEY, secret:GONE");
    raises(() => read("GONE"), "OSError", "the host holds no value for GONE");
    raises(() => read(7), "ValueError", "secret.read takes a name as a str");
    if (JSON.stringify(asked) !== '["API_KEY","GONE"]') throw new Error(`the host was asked for ${asked}`);
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

Deno.test("system: net reads a url as every parser does, and an @ after the host is its path", async () => {
    const server = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen() {} }, (req) => new Response(new URL(req.url).pathname));
    const at = `http://127.0.0.1:${server.addr.port}`;
    const main = net("main", ["127.0.0.1"]);
    for (const url of ["http://evil.test\\@127.0.0.1/", "http://user@127.0.0.1/", `${at}@evil.test/`, `${at}/a b`, "http://12%37.0.0.1/", "127.0.0.1/", "http://[::1]/", "http://0x7f.1/", "http://2130706433/"]) {
        raises(() => main.calls.request("GET", url), "ValueError", `'${url}' is not a plain url, net takes scheme://host/path with no user, backslash or space`);
    }
    const id = main.calls.request("GET", `${at}/@dylan`, [["accept", "text/plain"]]);
    const [status] = await main.calls.response(id);
    const path = new TextDecoder().decode(await main.calls.read(id));
    main.close();
    await server.shutdown();
    if (status !== 200 || path !== "/@dylan") throw new Error(`unexpected ${status} ${path}`);
});

Deno.test("system: a redirect is refused, the next address goes through its own request", async () => {
    const server = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen() {} }, () => new Response(null, { status: 302, headers: { location: "http://evil.example/" } }));
    const at = `http://127.0.0.1:${server.addr.port}/`;
    const main = net("main", ["127.0.0.1"]);
    const id = main.calls.request("GET", at);
    try {
        await main.calls.response(id);
        throw new Error("the redirect was followed");
    } catch (e) {
        if (e.name !== "OSError" || e.message !== `net.request to ${at} was redirected, request the new address with its own net.request`) throw e;
    }
    main.close();
    await server.shutdown();
});

Deno.test("system: the headers fetch keeps for the host are refused", () => {
    const main = net("main", ["127.0.0.1"]);
    for (const name of ["Host", "cookie", "Origin", "Content-Length", "Sec-Fetch-Site", "Proxy-Authorization"]) {
        raises(() => main.calls.request("GET", "http://127.0.0.1:1/", [[name, "x"]]), "ValueError", `the header '${name}' belongs to the host, a program cannot set it`);
    }
    main.close();
});

Deno.test("system: a path scope reaches only under its prefix", () => {
    const api = net("main", ["a.test/api", "b.test"]).calls;
    const held = "edge.json grants it net:a.test/api, net:b.test";
    for (const path of ["/api", "/api/", "/api/items", "/api/v2/items?q=1"]) api.request("GET", `http://a.test${path}`);
    api.request("GET", "http://b.test/anything");
    denied(() => api.request("GET", "http://a.test/"), `'main' has no net:a.test/, ${held}`);
    denied(() => api.request("GET", "http://a.test/apixyz"), `'main' has no net:a.test/apixyz, ${held}`);
    denied(() => api.request("GET", "http://a.test/other/api"), `'main' has no net:a.test/other/api, ${held}`);
    // A host no scope names reads as the host alone, whatever path was asked for.
    denied(() => api.request("GET", "http://c.test/api"), `'main' has no net:c.test, ${held}`);
    // A prefix is climbed out of by no spelling of the segment above.
    for (const path of ["/api/../secret", "/api/%2e%2e/secret", "/api/.%2e/secret", "/api/./../secret"]) {
        denied(() => api.request("GET", `http://a.test${path}`), `'main' has no net:a.test/secret, ${held}`);
    }
    // A step a server may still take, by decoding once or twice or dropping a parameter, is refused.
    for (const path of ["/api/..%2fsecret", "/api/%2E%2E%2Fsecret", "/api/..;/secret", "/api/%252e%252e/secret"]) {
        denied(() => api.request("GET", `http://a.test${path}`), `'main' has no net:a.test${path}, ${held}`);
    }
    // An escape that stays under the prefix in every reading reaches it.
    for (const path of ["/api/a%5cb", "/api/x%2e", "/api/group%2Fproject", "/%61pi/x"]) api.request("GET", `http://a.test${path}`);
    net("main", ["a.test/api/"]).calls.request("GET", "http://a.test/api");
});

Deno.test("system: both hosts send one reading of a path", async () => {
    const seen = [];
    const server = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen() {} }, (req) => {
        seen.push(new URL(req.url).pathname + new URL(req.url).search);
        return new Response("ok");
    });
    const at = `http://127.0.0.1:${server.addr.port}`;
    const main = net("main", ["127.0.0.1"]);
    const sent = async (url) => {
        const id = main.calls.request("GET", at + url);
        await main.calls.response(id);
        return seen.pop();
    };
    const cases = [
        ["/a/b/../c", "/a/c"],
        ["/a/./b", "/a/b"],
        ["/a/b/..", "/a/"],
        ["/..", "/"],
        ["/a/%2e%2e/b", "/b"],
        ["/Español", "/Espa%C3%B1ol"],
        ["/@dylan", "/@dylan"],
        ["/items?q=a/../b", "/items?q=a/../b"],
        ["/keep#gone", "/keep"],
        // No count of dot segments climbs above the root into the host.
        ["/../@evil.test/x", "/@evil.test/x"],
        ["/%2E%2E/.evil.test/x", "/.evil.test/x"],
    ];
    for (const [asked, want] of cases) {
        const got = await sent(asked);
        if (got !== want) throw new Error(`${asked} reached ${got}, want ${want}`);
    }
    main.close();
    await server.shutdown();
});
