import { fetchModule } from './fetch.ts';
import { loadNativeModule, nativeTable } from './native.ts';
import type { CompilerExports } from './wasm.ts';
import type { Rt } from './rt.ts';
import { MODULES } from './system/names.ts';
import type { Permissions } from './system/grants.ts';
import { errMsg, writeBytes } from './util.ts';

const TD = new TextDecoder();
const TE = new TextEncoder();

interface PrefetchCtx {
    fetchedSources: Map<string, Uint8Array>
    knownMissing: Set<string>
    importsMap?: Record<string, string> | null
    permissions?: Permissions | null
    baseUrl?: string | null
    read?: (url: string) => Promise<Response>
    compilerExports: CompilerExports
    rt: Rt
}

/* Who a run's modules belong to, each manifest dir with its package, and what the root grants. */
export interface Packages {
    dirs: [string, string][]
    root: string
    permissions: Permissions
    // Whether anything can reach a system module, a name no manifest declared or a plugin.
    needed: boolean
}

// What the engine's walk asks of this host next, as it writes it into the out buffer.
type Step = { fetch: string } | { plugin: string, name: string } | { system: Packages } | { undeclared: string[] } | { done: string[] };

/* Hint when a module spec likely can't load, insecure scheme or schemeless URL. Null when it looks fine. */
function schemeHint(spec: string): string | null {
    if (spec.startsWith('http://')) {
        return `'${spec}' uses http://; browsers block http subresources from an https page `
             + `(mixed content), so the fetch never leaves. Use https:// (an SSL connection).`;
    }
    // No scheme but a dotted first segment looks like a domain, yet the host treats it as a relative path.
    const relative = spec.startsWith('.') || spec.startsWith('/') || spec.includes('://');
    const firstSegment = spec.split('/')[0];
    if (!relative && firstSegment !== undefined && firstSegment.includes('.')) {
        return `'${spec}' has no scheme, so it resolved as a path on your own origin. `
             + `If it's a URL, prefix it with https://.`;
    }
    return null;
}

/* The engine walks what a program imports, this host fetches what it asks for, loads the plugins it names and serves the system modules through `serve`. */
export async function bfsPrefetch(rootSrc: string, exports: CompilerExports, ctx: PrefetchCtx, serve: (packages: Packages) => string[]): Promise<void> {
    const { fetchedSources, knownMissing, importsMap, permissions } = ctx;

    // What the embedder declared stands in for the root edge.json, resolved like any manifest.
    if ((importsMap && Object.keys(importsMap).length > 0) || permissions) {
        fetchedSources.set('edge.json', TE.encode(JSON.stringify({ imports: importsMap ?? {}, ...(permissions && { permissions }) })));
        knownMissing.delete('edge.json');
    }

    // A walk export takes staged buffers, freed after, and leaves the next step in the out buffer.
    const call = (buffers: Uint8Array[], run: (at: number[]) => number): Step => {
        const at = buffers.map((bytes) => writeBytes(exports, bytes));
        let len: number;
        try { len = run(at); } finally { buffers.forEach((bytes, i) => exports.wasm_free(at[i]!, Math.max(1, bytes.length))); }
        return JSON.parse(TD.decode(new Uint8Array(exports.memory.buffer, exports.out_ptr(), len))) as Step;
    };

    const [src, system] = [TE.encode(rootSrc), TE.encode(MODULES.join('\n'))];
    let step = call([src, system], ([s, m]) => exports.walk_start(s!, src.length, m!, system.length));

    for (;;) {
        if ('fetch' in step) {
            const spec = step.fetch;
            let bytes = fetchedSources.get(spec);
            if (bytes === undefined && !knownMissing.has(spec)) {
                bytes = (await fetchModule(spec, ctx)) ?? undefined;
                if (bytes) fetchedSources.set(spec, bytes);
            }
            // A manifest or lock is only probed, so only a missing module earns a hint.
            const probe = spec.endsWith('edge.json') || spec.endsWith('edge.lock');
            const [answer, kind] = bytes ? [bytes, 0] : [TE.encode(probe ? '' : schemeHint(spec) ?? ''), 1];
            step = call([answer], ([at]) => exports.walk_fetched(at!, answer.length, kind));
        } else if ('plugin' in step) {
            const len = exports.walk_plugin_bytes();
            const bytes = new Uint8Array(exports.memory.buffer, exports.out_ptr(), len).slice();
            let failed = '';
            try {
                const { names, fns } = await loadNativeModule(bytes, exports);
                const baseId = nativeTable.length;
                for (const fn of fns) nativeTable.push(fn);
                const [spec, listed] = [TE.encode(step.plugin), TE.encode(names.join('\n'))];
                const [s, n] = [writeBytes(exports, spec), writeBytes(exports, listed)];
                exports.register_native_module(s, spec.length, n, listed.length, baseId);
                exports.wasm_free(s, Math.max(1, spec.length));
                exports.wasm_free(n, Math.max(1, listed.length));
            } catch (e) {
                failed = `'${step.plugin}' failed to load as a wasm module: ${errMsg(e)}`;
            }
            const why = TE.encode(failed);
            step = call([why], ([at]) => exports.walk_plugin(failed ? 1 : 0, at!, why.length));
        } else if ('system' in step) {
            const failures = TE.encode(serve(step.system).join('\0'));
            step = call([failures], ([at]) => exports.walk_served(at!, failures.length));
        } else if ('undeclared' in step) {
            // A page has no `edge add` to point at, so every undeclared name keeps the generic help.
            const none = new Uint8Array();
            step = call([none], ([at]) => exports.walk_known(at!, 0));
        } else {
            if (step.done.length) throw new Error(`could not pre-fetch every imported module:\n  ${step.done.join('\n  ')}`);
            return;
        }
    }
}
