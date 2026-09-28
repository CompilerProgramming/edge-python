import type { EdgeValue } from '../rt.ts';
import { batched } from './batch.ts';
import { SystemError } from './error.ts';
import { need } from './grants.ts';

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

const pairs = (value: EdgeValue): [string, string][] => {
    if (value === null || value === undefined) return [];
    const entries = Array.isArray(value) ? value : Object.entries(value as Record<string, EdgeValue>);
    return entries.map((pair) => {
        if (!Array.isArray(pair) || pair.length !== 2) throw new SystemError('ValueError', 'headers are [name, value] pairs or a dict');
        return [text(pair[0] as EdgeValue, 'a header name'), text(pair[1] as EdgeValue, 'a header value')];
    });
};

/* The host part of an absolute url, what a net scope names, read here since not every host has URL. */
const hostOf = (url: string): string => {
    const authority = /^[a-z][a-z0-9+.-]*:\/\/([^/?#]*)/i.exec(url)?.[1] ?? '';
    const host = authority.slice(authority.lastIndexOf('@') + 1);
    const name = host.startsWith('[') ? host.slice(0, host.indexOf(']') + 1) : host.split(':')[0] ?? '';
    if (!name) throw new SystemError('ValueError', `'${url}' is not an absolute url`);
    return name.toLowerCase();
};

const failed = (what: string, e: unknown) => new SystemError('OSError', `${what} failed, ${e instanceof Error ? e.message : String(e)}`);

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
        const target = text(url, 'a url');
        need(pkg, 'net', held, hostOf(target));
        const controller = new AbortController();
        const answer = fetch(target, { method: text(method ?? 'GET', 'a method'), headers: pairs(headers), body: body(content), signal: controller.signal });
        const head = answer.then((res): [number, [string, string][]] => {
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
        const target = text(url, 'a url');
        need(pkg, 'net', held, hostOf(target));
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
