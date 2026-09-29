import type { EdgeValue } from '../rt.ts';
import { batched } from './batch.ts';
import { SystemError } from './error.ts';
import { HOST, reach } from './grants.ts';

type Message = Uint8Array | string | null;

/* What an id answers to, a request in flight or an open socket. */
type Stream =
    | { kind: 'request', abort: () => void, head: Promise<[number, [string, string][]]>, body: Promise<ReadableStreamDefaultReader<Uint8Array> | null> }
    | { kind: 'socket', abort: () => void, socket: WebSocket, messages: Message[], waiting: ((message: Message) => void)[] };

let next = 1;

const text = (value: EdgeValue, what: string): string => {
    if (typeof value !== 'string') throw new SystemError('ValueError', `${what} must be a str`);
    return value;
};

const body = (value: EdgeValue): Uint8Array<ArrayBuffer> | string | undefined => {
    if (value === null || value === undefined) return undefined;
    if (value instanceof Uint8Array) return value as Uint8Array<ArrayBuffer>;
    if (typeof value === 'string') return value;
    throw new SystemError('ValueError', 'a body must be bytes, a str or None');
};

// The headers fetch keeps for the host, refused rather than dropped so both hosts answer alike.
const FORBIDDEN = new Set([
    'accept-charset', 'accept-encoding', 'access-control-request-headers', 'access-control-request-method', 'connection', 'content-length',
    'cookie', 'cookie2', 'date', 'dnt', 'expect', 'host', 'keep-alive', 'origin', 'referer', 'set-cookie', 'te', 'trailer',
    'transfer-encoding', 'upgrade', 'via',
]);

const pairs = (value: EdgeValue): [string, string][] => {
    if (value === null || value === undefined) return [];
    const entries = Array.isArray(value) ? value : Object.entries(value as Record<string, EdgeValue>);
    return entries.map((pair) => {
        if (!Array.isArray(pair) || pair.length !== 2) throw new SystemError('ValueError', 'headers are [name, value] pairs or a dict');
        const name = text(pair[0] as EdgeValue, 'a header name');
        if (FORBIDDEN.has(name.toLowerCase()) || /^(?:proxy|sec)-/i.test(name)) throw new SystemError('ValueError', `the header '${name}' belongs to the host, a program cannot set it`);
        return [name, text(pair[1] as EdgeValue, 'a header value')];
    });
};

// A url every parser reads alike, a scheme, the host right after it, a port, then the rest.
const PLAIN = new RegExp(`^((?:https?|wss?)://)(${HOST.source})((?::\\d{1,5})?)([/?#].*)?$`, 'i');

// A segment standing for this directory or the one above it, spelled plainly or escaped.
const HERE = /^(?:\.|%2e)$/i;
const ABOVE = /^(?:\.|%2e){2}$/i;

// What a path may carry as written, everything else crosses as its UTF-8 bytes escaped.
const KEPT = /^[a-z0-9\-._~!$&'()*+,;=:@/%]$/i;

/* A path as every host sends it, its dot segments resolved and its text escaped, so one reading of it cannot hide another. */
const clean = (path: string): string => {
    const out: string[] = [];
    // The root is no segment, so no count of dot segments climbs above it into the host.
    const segments = path.replace(/^\//, '').split('/');
    for (const [i, segment] of segments.entries()) {
        const last = i === segments.length - 1;
        if (ABOVE.test(segment)) {
            out.pop();
            if (last) out.push('');
        } else if (HERE.test(segment)) {
            if (last) out.push('');
        } else {
            out.push([...segment].map((c) => (KEPT.test(c) ? c : escaped(c))).join(''));
        }
    }
    return `/${out.join('/')}`;
};

/* One character as the percent escapes of its UTF-8 bytes, spelled out since not every host has TextEncoder. */
const escaped = (c: string): string => {
    const cp = c.codePointAt(0) ?? 0;
    const bytes = cp < 0x80 ? [cp]
        : cp < 0x800 ? [0xc0 | (cp >> 6), 0x80 | (cp & 0x3f)]
        : cp < 0x10000 ? [0xe0 | (cp >> 12), 0x80 | ((cp >> 6) & 0x3f), 0x80 | (cp & 0x3f)]
        : [0xf0 | (cp >> 18), 0x80 | ((cp >> 12) & 0x3f), 0x80 | ((cp >> 6) & 0x3f), 0x80 | (cp & 0x3f)];
    return bytes.map((b) => `%${b.toString(16).toUpperCase().padStart(2, '0')}`).join('');
};

/* The host a plain url names, null when it is not one. */
const hostIn = (url: string): string | null => {
    const parts = [...url].every((c) => c > ' ' && c !== '\\' && c !== '\x7f') ? PLAIN.exec(url) : null;
    return parts ? parts[2]!.toLowerCase() : null;
};

/* A url as both hosts send it, refused when anything could let a parser read another host or path. */
const plain = (url: string): { host: string, path: string, url: string } => {
    const refused = () => new SystemError('ValueError', `'${url}' is not a plain url, net takes scheme://host/path with no user, backslash or space`);
    const parts = hostIn(url) === null ? null : PLAIN.exec(url);
    if (!parts) throw refused();
    const [, scheme, name, port, rest = '/'] = parts;
    // A fragment never leaves the host, so it is no part of what a grant reaches.
    const [addressed] = rest.split('#') as [string];
    const at = addressed.indexOf('?');
    const path = clean(at === -1 ? addressed : addressed.slice(0, at));
    const query = at === -1 ? '' : addressed.slice(at);
    const host = name!.toLowerCase();
    const sent = `${scheme!.toLowerCase()}${host}${port}${path}${query}`;
    // The url is rebuilt, so it must still name the host that was checked, whatever the path became.
    if (hostIn(sent) !== host) throw refused();
    return { host, path, url: sent };
};

const failed = (what: string, e: unknown) => new SystemError('OSError', `${what} failed, ${e instanceof Error ? e.message : String(e)}`);

// Statuses that hand a request elsewhere, which only a new request of its own may follow.
const REDIRECTS = [301, 302, 303, 307, 308];

async function chunk(head: Promise<unknown>, body: Promise<ReadableStreamDefaultReader<Uint8Array> | null>): Promise<Message> {
    await head;
    const reader = await body;
    if (!reader) return null;
    try {
        const { done, value } = await reader.read();
        return done ? null : value;
    } catch (e) {
        throw failed('reading the response', e);
    }
}

/* The net calls of one package, each reaching only the hosts it holds and the ids it opened. */
export default function net(pkg: string, held: string[]) {
    const streams = new Map<number, Stream>();

    const stream = (id: EdgeValue): Stream => {
        const found = typeof id === 'number' ? streams.get(id) : undefined;
        if (!found) throw new SystemError('ValueError', `no open request or socket ${String(id)}`);
        return found;
    };

    /* Starts a request and returns its id at once, the head and the body arrive through response and read. */
    function request(method: EdgeValue, url: EdgeValue, headers: EdgeValue = null, content: EdgeValue = null): number {
        const { host, path, url: target } = plain(text(url, 'a url'));
        reach(pkg, held, host, path);
        const controller = new AbortController();
        const answer = fetch(target, { method: text(method ?? 'GET', 'a method'), headers: pairs(headers), body: body(content), signal: controller.signal, redirect: 'manual' });
        const head = answer.then((res): [number, [string, string][]] => {
            // No host follows a redirect, a browser hides where it points and a new request checks it.
            if (res.type === 'opaqueredirect' || REDIRECTS.includes(res.status)) {
                throw new SystemError('OSError', `net.request to ${target} was redirected, request the new address with its own net.request`);
            }
            const received: [string, string][] = [];
            res.headers.forEach((value, name) => received.push([name, value]));
            return [res.status, received];
        }, (e) => { throw failed(`net.request to ${target}`, e); });
        // A request nobody reads still settles, so its failure never surfaces as unhandled.
        head.catch(() => {});
        const id = next++;
        streams.set(id, { kind: 'request', abort: () => controller.abort(), head, body: answer.then((res) => res.body?.getReader() ?? null, () => null) });
        return id;
    }

    /* The status and headers of a request, once they arrived. */
    function response(id: EdgeValue): Promise<[number, [string, string][]]> {
        const found = stream(id);
        if (found.kind !== 'request') throw new SystemError('ValueError', 'response takes a request from net.request');
        return found.head;
    }

    /* The next chunk of a body or message of a socket, None once it ended. */
    function read(id: EdgeValue): Message | Promise<Message> {
        const found = stream(id);
        if (found.kind === 'request') return chunk(found.head, found.body);
        if (found.messages.length > 0) return found.messages.shift() ?? null;
        return new Promise((resolve) => found.waiting.push(resolve));
    }

    /* Opens a WebSocket and returns its id once it is open, its messages arrive through read. */
    function connect(url: EdgeValue): Promise<number> {
        const { host, path, url: target } = plain(text(url, 'a url'));
        reach(pkg, held, host, path);
        const socket = new WebSocket(target);
        socket.binaryType = 'arraybuffer';
        const found: Stream = { kind: 'socket', abort: () => socket.close(), socket, messages: [], waiting: [] };
        const deliver = (message: Message) => {
            const waiter = found.waiting.shift();
            if (waiter) waiter(message);
            else found.messages.push(message);
        };
        socket.onmessage = (e) => deliver(typeof e.data === 'string' ? e.data : new Uint8Array(e.data as ArrayBuffer));
        socket.onclose = () => deliver(null);
        const id = next++;
        streams.set(id, found);
        return new Promise((resolve, reject) => {
            socket.onopen = () => resolve(id);
            socket.onerror = () => {
                streams.delete(id);
                reject(new SystemError('OSError', `the socket to ${target} failed`));
            };
        });
    }

    /* Sends bytes or a str on an open socket. */
    function send(id: EdgeValue, data: EdgeValue): null {
        const found = stream(id);
        if (found.kind !== 'socket') throw new SystemError('ValueError', 'send takes a socket from net.connect');
        if (found.socket.readyState !== WebSocket.OPEN) throw new SystemError('OSError', 'the socket is not open');
        found.socket.send(body(data) ?? new Uint8Array(0));
        return null;
    }

    /* Aborts a request or closes a socket. */
    function close(id: EdgeValue): null {
        stream(id).abort();
        streams.delete(id as number);
        return null;
    }

    /* Aborts what a finished run left open. */
    function closeAll(): void {
        for (const found of streams.values()) found.abort();
        streams.clear();
    }

    const calls = { request, response, read, connect, send, close };
    return { calls: { ...calls, batch: batched('net', calls) }, close: closeAll };
}
