import net from './net.ts';
import secret from './secret.ts';
import time from './time.ts';
import type { Module } from './names.ts';

/* What a host lends the system calls, a value it keeps for the program, asked for by one name. */
export type Host = { secret(name: string): string | null };

/* Every system module, each opened per package with the scopes that package holds, one per name MODULES lists. */
export const SYSTEM = { net, secret, time } satisfies Record<Module, (pkg: string, held: string[], host: Host) => unknown>;
