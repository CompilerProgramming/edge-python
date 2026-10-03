/* The few web APIs the system calls use and SpiderMonkey lacks, each over the host's pipe, and the bridge the host calls in through. */
(() => {
    const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    const unhex = (text) => Uint8Array.from({ length: text.length / 2 }, (_, i) => parseInt(text.substr(i * 2, 2), 16));
    const marked = (value) => value !== null && typeof value === "object" && !Array.isArray(value) && Object.keys(value).length === 1;

    // JSON carries neither a big int nor bytes, so both cross as a one-key object.
    const encode = (_key, value) => {
        if (typeof value === "bigint") return { $int: value.toString() };
        if (value instanceof Uint8Array) return { $bytes: hex(value) };
        if (typeof value === "number" && !Number.isFinite(value)) return { $float: String(value) };
        return value;
    };
    const decode = (_key, value) => {
        if (!marked(value)) return value;
        if ("$int" in value) return BigInt(value.$int);
        if ("$bytes" in value) return unhex(value.$bytes);
        if ("$float" in value) return Number(value.$float);
        return value;
    };
    const fault = (e) => [e?.name ?? "Error", e?.message ?? String(e)];

    const call = (op, value) => {
        const answer = JSON.parse(__pipe(op, JSON.stringify(value, encode)), decode);
        if (answer.error) throw new TypeError(answer.error);
        return answer.value;
    };

    // Pipe operations still in flight, each settled when the host delivers its answer.
    const waiting = new Map();
    let next = 1;
    const wait = (op, value) => new Promise((resolve, reject) => {
        const token = next++;
        waiting.set(token, { resolve, reject });
        call(op, { ...value, token });
    });
    globalThis.__edge_io = (json) => {
        const { token, value, error } = JSON.parse(json, decode);
        const settle = waiting.get(token);
        waiting.delete(token);
        if (error) settle?.reject(new TypeError(error));
        else settle?.resolve(value);
        return "{}";
    };

    globalThis.performance = { now: () => call("monotonic", {}) };

    globalThis.AbortController = class AbortController {
        constructor() {
            const listeners = [];
            this.signal = { aborted: false, addEventListener: (_type, listener) => listeners.push(listener) };
            this.abort = () => {
                if (this.signal.aborted) return;
                this.signal.aborted = true;
                for (const listener of listeners) listener();
            };
        }
    };

    globalThis.fetch = async (url, init = {}) => {
        const id = call("http_start", { method: init.method ?? "GET", url: String(url), headers: init.headers ?? [], body: init.body ?? null });
        init.signal?.addEventListener("abort", () => call("http_abort", { id }));
        const [status, headers] = await wait("http_head", { id });
        let ended = false;
        const reader = {
            read: async () => {
                const value = ended ? null : await wait("http_chunk", { id });
                ended = value === null;
                return ended ? { done: true, value: undefined } : { done: false, value };
            },
        };
        return { status, headers: { forEach: (visit) => headers.forEach(([name, value]) => visit(value, name)) }, body: { getReader: () => reader } };
    };

    globalThis.WebSocket = class WebSocket {
        static OPEN = 1;
        constructor(url) {
            this.readyState = 0;
            this.binaryType = "arraybuffer";
            const shut = (listener) => {
                this.readyState = 3;
                listener?.();
            };
            wait("ws_open", { url: String(url) }).then((id) => {
                this.id = id;
                this.readyState = 1;
                this.onopen?.();
                const pump = () => wait("ws_next", { id }).then((message) => {
                    if (message === null) return shut(this.onclose);
                    this.onmessage?.({ data: typeof message === "string" ? message : message.buffer });
                    pump();
                }, () => shut(this.onclose));
                pump();
            }, () => shut(this.onerror));
        }
        send(data) {
            call("ws_send", { id: this.id, data });
        }
        close() {
            if (this.id !== undefined && this.readyState === 1) call("ws_close", { id: this.id });
            this.readyState = 3;
        }
    };

    // The system modules opened for a run, each keyed by the run, its package and the module.
    const opened = new Map();
    // A secret crosses only when a granted name asks for it, read from the environment at that moment, and a file only from the project of its own run.
    const hostOf = (run) => ({
        secret: (name) => call("secret", { name }),
        read: (path, limit) => call("fs_read", { run, path, limit }),
        list: (dir, limit) => call("fs_list", { run, dir, limit }),
    });
    globalThis.__edge_check = (json) => JSON.stringify({ error: globalThis.__edge.check(JSON.parse(json)) });
    globalThis.__edge_held = (json) => JSON.stringify(globalThis.__edge.held(JSON.parse(json)));
    globalThis.__edge_scopes = (json) => {
        const { chain, module } = JSON.parse(json);
        return JSON.stringify(globalThis.__edge.scopes(globalThis.__edge.held(chain), module));
    };
    globalThis.__edge_unmet = (json) => {
        const { held, section } = JSON.parse(json);
        return JSON.stringify(globalThis.__edge.unmet(held, section));
    };
    globalThis.__edge_open = (json) => {
        const { key, run, module, pkg, held } = JSON.parse(json);
        const system = globalThis.__edge.open[module](pkg, held, hostOf(run));
        opened.set(key, system);
        return JSON.stringify(Object.keys(system.calls));
    };
    globalThis.__edge_close = (json) => {
        const { run } = JSON.parse(json);
        for (const [key, system] of opened) {
            if (!key.startsWith(`${run}:`)) continue;
            system.close();
            opened.delete(key);
        }
        return "{}";
    };
    globalThis.__edge_invoke = (json) => {
        const { key, name, args, call: id } = JSON.parse(json, decode);
        try {
            const result = opened.get(key).calls[name](...args);
            if (!(result instanceof Promise)) return JSON.stringify({ value: result }, encode);
            result.then(
                (value) => __pipe("settle", JSON.stringify({ call: id, value }, encode)),
                (e) => __pipe("settle", JSON.stringify({ call: id, error: fault(e) })),
            );
            return JSON.stringify({ pending: true });
        } catch (e) {
            return JSON.stringify({ error: fault(e) });
        }
    };
})();
