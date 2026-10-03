import { SystemError } from './error.ts';

/* A permissions section of an edge.json, each holder to its entries, `module` or `module:scope`. */
export type Permissions = Record<string, string[]>;

/* Each section from the root to a package, beside the key the next comes in under. */
export type Chain = [Permissions, string][];

// A host as a net scope names it and a url reaches it, a dotted lowercase name.
export const HOST = /[a-z0-9.-]+/;

// An ip4 in the one spelling every parser reads alike, four decimal parts with no leading zero.
const OCTET = '(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)';
const IP4 = new RegExp(`^${OCTET}(?:\\.${OCTET}){3}$`);

/* Whether a host reads as one address everywhere, since a browser takes any host ending in a number for an ip4. */
export function plainHost(host: string): boolean {
    const last = host.split('.').filter(Boolean).pop() ?? '';
    return !/^(?:\d+|0x[0-9a-f]*)$/i.test(last) || IP4.test(host);
}

// A whole scope for net, one lowercase host and the path prefix it may bound the reach to.
const NET_SCOPE = new RegExp(`^(${HOST.source})((?:/[A-Za-z0-9\\-._~!$&'()*+,=:@]+)*/?)$`);

// A segment of a project path, never empty, never hidden behind a dot and free of what a host reads as a separator.
const plainSegment = (part: string) => part !== '' && !part.startsWith('.') && [...part].every((c) => c >= ' ' && c !== '\\' && c !== '\x7f');

/* A path under the root edge.json as both hosts read it, '' for the root itself, null when it could leave it. */
export function plainPath(path: string): string | null {
    if (path === '.') return '';
    const parts = (path.startsWith('./') ? path.slice(2) : path).split('/');
    return parts.every(plainSegment) ? parts.join('/') : null;
}

// What a scope of each system module may be, a folder for fs, a host for net, a name for secret and a clock for time.
const SCOPES: Record<string, (scope: string) => boolean> = {
    // Written from the root edge.json, so a grant always starts at it.
    fs: (dir) => dir === '.' || (dir.startsWith('./') && plainPath(dir) !== null),
    // A prefix names plain segments, since a dot segment or an escape never matches a resolved path.
    net: (scope) => {
        const [, host, prefix] = NET_SCOPE.exec(scope) ?? [];
        return host !== undefined && plainHost(host) && prefix!.split('/').every((segment) => segment !== '.' && segment !== '..');
    },
    // Spelled as an environment variable, since the CLI reads the one variable a name forms.
    secret: (name) => /^[A-Z_][A-Z0-9_]*$/.test(name),
    time: (clock) => clock === 'wall' || clock === 'monotonic' || clock === 'zone',
};

const split = (entry: string): [string, string | null] => {
    const at = entry.indexOf(':');
    return at === -1 ? [entry, null] : [entry.slice(0, at), entry.slice(at + 1)];
};

/* Why a permissions section is malformed, null when every holder lists known entries. */
export function check(section: unknown): string | null {
    if (section === undefined) return null;
    if (typeof section !== 'object' || section === null || Array.isArray(section)) return 'permissions must map each package to a list of entries';
    for (const [holder, entries] of Object.entries(section)) {
        if (!Array.isArray(entries) || !entries.every((e) => typeof e === 'string')) {
            return `permissions for '${holder}' must be a list of entries such as "net:api.example.com"`;
        }
        for (const [module, scope] of entries.map(split)) {
            const valid = SCOPES[module];
            if (!valid) return `permissions for '${holder}' name '${module}', which is not a system module (${Object.keys(SCOPES).join(', ')})`;
            if (scope !== null && !valid(scope)) return `permissions for '${holder}' give ${module} the scope '${scope}', which it does not have`;
        }
    }
    return null;
}

// A bare module asks only to import it, which any entry for it covers.
const covers = (held: string[], entry: string): boolean => {
    const [module, scope] = split(entry);
    return scope === null ? held.some((e) => split(e)[0] === module) : held.includes(entry);
};

/* What the end of `chain` holds, no importer past the root passing on more than it holds. */
export function held(chain: Chain): string[] {
    let held: string[] | null = null;
    for (const [section, key] of chain) {
        const given = [...(section['all'] ?? []), ...(section[key] ?? [])];
        held = held === null ? given : given.filter((entry) => covers(held!, entry));
    }
    return [...new Set(held ?? [])];
}

/* The scopes `held` gives `module`, null when no entry names it. */
export function scopes(held: string[], module: string): string[] | null {
    let found: string[] | null = null;
    for (const [name, scope] of held.map(split)) {
        if (name !== module) continue;
        found ??= [];
        if (scope !== null) found.push(scope);
    }
    return found;
}

/* Each entry of `section` that `held` misses, since a package needs what it passes on too. */
export function unmet(held: string[], section: Permissions): string[] {
    return [...new Set(Object.values(section).flat())].filter((entry) => !covers(held, entry));
}

/* Raises PermissionError unless `held` holds `scope`, naming what the package was granted instead. */
export function need(pkg: string, module: string, held: string[], scope: string): void {
    if (held.includes(scope)) return;
    const granted = held.length > 0 ? held.map((s) => `${module}:${s}`).join(', ') : 'nothing';
    throw new SystemError('PermissionError', `'${pkg}' has no ${module}:${scope}, edge.json grants it ${granted}`);
}

// A segment standing for this directory or the one above it, spelled plainly or escaped.
const HERE = /^(?:\.|%2e)$/i;
const ABOVE = /^(?:\.|%2e){2}$/i;

/* A path with its dot segments applied however they are spelled, never above the root. */
export function resolve(path: string): string {
    const out: string[] = [];
    const segments = path.replace(/^\//, '').split('/');
    for (const [i, segment] of segments.entries()) {
        const above = ABOVE.test(segment);
        if (above) out.pop();
        if (!above && !HERE.test(segment)) out.push(segment);
        else if (i === segments.length - 1) out.push('');
    }
    return `/${out.join('/')}`;
}

// An escape a server may undo before it routes, the unreserved ones every server decodes.
const ESCAPE = /%([0-9a-f]{2})/gi;
const UNRESERVED = /[A-Za-z0-9\-._~]/;
const byte = (hex: string) => String.fromCharCode(parseInt(hex, 16));
const decode = (text: string, only = /[^]/) => text.replace(ESCAPE, (escape, hex: string) => (only.test(byte(hex)) ? byte(hex) : escape));

/* Every path a server might route `path` to, a backslash taken for a slash and a parameter dropped, decoded as far as twice. */
const readings = (path: string): string[] =>
    [decode(path, UNRESERVED), decode(path), decode(decode(path))].map((read) => resolve(read.replace(/\\/g, '/').replace(/;[^/]*/g, '')));

/* Raises PermissionError unless a held scope reaches `host` at `path`, a scope with a path prefix reaching only under it. */
export function reach(pkg: string, held: string[], host: string, path: string): void {
    const bounded = held.filter((scope) => scope.split('/')[0] === host);
    // A prefix holds only when every reading a server might take of the path stays under it.
    const under = (prefix: string) => readings(path).every((read) => read === prefix || read.startsWith(`${prefix}/`));
    if (bounded.some((scope) => scope === host || under(scope.slice(host.length).replace(/\/$/, '')))) return;
    // Naming the path only once the host is held keeps a refused host reading as the host alone.
    need(pkg, 'net', held, bounded.length > 0 ? `${host}${path}` : host);
}
