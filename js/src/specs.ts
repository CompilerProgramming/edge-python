export const sha256Hex = async (bytes: Uint8Array): Promise<string> => {
    const digest = await crypto.subtle.digest('SHA-256', bytes as BufferSource);
    return [...new Uint8Array(digest)].map(b => b.toString(16).padStart(2, '0')).join('');
};

// What `edge.lock` holds for one name, the address to fetch and the digest those bytes must have.
export interface Locked {
    version?: string
    url: string
    digest: string
}

// Mirror `cli::lock::version_of`, three numeric parts and nothing else reads as a release.
export const isVersion = (target: string): boolean => /^\d{1,9}\.\d{1,9}\.\d{1,9}$/.test(target);

/* What a declared target becomes once the lock has spoken, the pinned spec every host already reads. A path and an already pinned url stay themselves, so only a version has to be locked. JavaScript takes no pin, since the page imports it and cannot hash it. */
export const lockedSpec = (name: string, target: string, lock: Record<string, Locked> | null): string => {
    const entry = lock?.[name];
    if (isVersion(target)) {
        if (!entry) throw new Error(`'${name}' is not locked, run edge lock`);
        if (entry.version !== target) throw new Error(`'${name}' is declared ${target} and locked ${entry.version ?? 'a url'}, run edge lock`);
        return pinned(name, entry.url, entry.digest);
    }
    if (entry && target.includes('://') && !target.includes('#sha256-')) return pinned(name, entry.url, entry.digest);
    return target;
};

/* The url and digest spelled as a pinned spec, checked here so a hand-edited lock names itself rather than the address it holds. */
const pinned = (name: string, url: string, digest: string): string => {
    if (!/^sha256-[0-9a-f]{64}$/.test(digest)) throw new Error(`'${name}' holds '${digest}' in edge.lock, which is not sha256- and 64 hex characters`);
    return /\.m?js$/.test(url.replace(/[?#].*$/, '')) ? url : `${url}#${digest}`;
};

/* Mirror `compiler::modules::manifest` so transitive imports canonicalize identically on both sides. */
export const dirOf = (spec: string): string => {
    // A packed package is the directory of the files it carries.
    const path = spec.replace(/[?#].*$/, '');
    if (path.endsWith('.edge')) return path + '/';
    const i = spec.lastIndexOf('/');
    return i === -1 ? '' : spec.slice(0, i + 1);
};

export const parentDir = (dir: string): string | null => {
    if (dir === '') return null;
    const trimmed = dir.endsWith('/') ? dir.slice(0, -1) : dir;
    const sch = trimmed.indexOf('://');
    if (sch !== -1 && !trimmed.slice(sch + 3).includes('/')) return null;
    const i = trimmed.lastIndexOf('/');
    return i === -1 ? '' : trimmed.slice(0, i + 1);
};

export const joinRel = (base: string, target: string): string => {
    if (target.includes('://') || target.startsWith('/') || target.startsWith('mt:')) return target;
    if (base.includes('://')) return new URL(target, base).toString();
    let b = base, t = target;
    while (t.startsWith('../')) {
        const p = parentDir(b); b = p == null ? '' : p;
        t = t.slice(3);
    }
    if (t === '..') { const p = parentDir(b); return p == null ? '' : p; }
    if (t === '.' || t === '') return b;
    if (b !== '') {
        while (t.startsWith('./')) t = t.slice(2);
        if (!b.endsWith('/')) b += '/';
    }
    return b + t;
};
