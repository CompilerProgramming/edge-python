interface FetchCtx {
    baseUrl?: string | null
    // How the engine reaches a url, a room reads the program's own files through its page.
    read?: (url: string) => Promise<Response>
    knownMissing: Set<string>
}

// Specs are root-relative, the URL join clamps escapes at the origin.
const requestUrl = (target: string, baseUrl?: string | null): string =>
    target.includes('://') ? target : new URL(target, baseUrl ?? self.location.href).toString();

/* Fetches a module, whose #sha256- pin the engine checks. Null on a failed fetch or a non-ok status. */
export async function fetchModule(spec: string, ctx: FetchCtx): Promise<Uint8Array | null> {
    const { baseUrl, read = fetch, knownMissing } = ctx;

    // A #sha256- fragment stays in the spec the engine verifies, but never leaves in the request URL.
    const fragAt = spec.indexOf('#sha256-');
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

    return new Uint8Array(await resp.arrayBuffer());
}
