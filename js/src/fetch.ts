import { writeBytes } from './util.ts';
import type { CompilerExports } from './wasm.ts';

const TD = new TextDecoder();

export interface FetchCtx {
    baseUrl?: string | null
    // How the engine reaches a url, a room reads the program's own files through its page.
    read?: (url: string) => Promise<Response>
    knownMissing: Set<string>
    compilerExports: CompilerExports
}

/* The engine hashes a pin, since the worker a room starts has no crypto.subtle. */
function sha256Hex(exports: CompilerExports, bytes: Uint8Array): string {
    const ptr = writeBytes(exports, bytes);
    const len = exports.sha256_hex(ptr, bytes.length);
    exports.wasm_free(ptr, Math.max(1, bytes.length));
    return TD.decode(new Uint8Array(exports.memory.buffer, exports.out_ptr(), len));
}

// Specs are root-relative, the URL join clamps escapes at the origin.
export const requestUrl = (target: string, baseUrl?: string | null): string =>
    target.includes('://') ? target : new URL(target, baseUrl ?? self.location.href).toString();

/* Fetches a module and checks its #sha256- pin. Null on a failed fetch or a non-ok status, throws on a mismatch. */
export async function fetchModule(spec: string, ctx: FetchCtx): Promise<Uint8Array | null> {
    const { baseUrl, read = fetch, knownMissing } = ctx;

    // An explicit #sha256- fragment pins the bytes. It stays in the spec but leaves the request URL.
    const fragAt = spec.indexOf('#sha256-');
    const pin = fragAt === -1 ? null : spec.slice(fragAt + 8);
    const target = fragAt === -1 ? spec : spec.slice(0, fragAt);

    let resp: Response;
    try {
        resp = await read(requestUrl(target, baseUrl));
    } catch (e) {
        // A manifest probe is opportunistic, a module that fails to fetch is worth a warning.
        if (spec.endsWith('edge.json')) knownMissing.add(spec);
        else if (!spec.endsWith('edge.lock')) console.warn(`[edge-python] fetch failed for '${spec}':`, e);
        return null;
    }

    if (!resp.ok) {
        if (resp.status === 404 && spec.endsWith('edge.json')) knownMissing.add(spec);
        // An absent lock is reported where the version it would resolve is, and is never remembered, so writing one is enough.
        else if (!spec.endsWith('edge.lock')) console.warn(`[edge-python] ${resp.status} for '${spec}' at ${resp.url}`);
        return null;
    }

    // A .wasm answered with HTML/text is a schemeless spec resolved relative and hitting an SPA fallback, not a module.
    if (target.endsWith('.wasm')) {
        const ct = (resp.headers.get('content-type') || '').toLowerCase();
        if (ct.includes('html') || ct.startsWith('text/')) {
            console.warn(`[edge-python] '${spec}' served as '${ct || 'no content-type'}', not a wasm module`);
            return null;
        }
    }

    const bytes = new Uint8Array(await resp.arrayBuffer());

    if (pin) {
        const hash = sha256Hex(ctx.compilerExports, bytes);
        if (pin !== hash) {
            throw new Error(`[edge-python] integrity check failed for '${target}'\n expected sha256-${pin}\n got sha256-${hash}`);
        }
    }

    return bytes;
}
