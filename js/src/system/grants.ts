import { SystemError } from './error.ts';

/* The permissions section of the root edge.json, each holder to its entries, `module` or `module:scope`. */
export type Permissions = Record<string, string[]>;

// What a scope of each system module may be, a host for net and a clock for time.
const SCOPES: Record<string, (scope: string) => boolean> = {
    net: (host) => host.length > 0 && !/[\s/]/.test(host),
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

/* The scopes `pkg` holds of `module`, its own entries joined with those for all, null when neither names the module. */
export function scopes(permissions: Permissions, pkg: string, module: string): string[] | null {
    // Code outside any package holds nothing, not even what all packages hold.
    if (!pkg) return null;
    let held: string[] | null = null;
    for (const [name, scope] of [...(permissions['all'] ?? []), ...(permissions[pkg] ?? [])].map(split)) {
        if (name !== module) continue;
        held ??= [];
        if (scope !== null) held.push(scope);
    }
    return held;
}

/* Raises PermissionError unless `held` holds `scope`, naming what the package was granted instead. */
export function need(pkg: string, module: string, held: string[], scope: string): void {
    if (held.includes(scope)) return;
    const granted = held.length > 0 ? held.map((s) => `${module}:${s}`).join(', ') : 'nothing';
    throw new SystemError('PermissionError', `'${pkg}' has no ${module}:${scope}, edge.json grants it ${granted}`);
}
