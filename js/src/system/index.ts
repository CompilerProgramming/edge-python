import fs from './fs.ts';
import net from './net.ts';
import secret from './secret.ts';
import time from './time.ts';
import type { Module } from './names.ts';

/* What a host lends the system calls, a value it keeps for the program, and the project files under its root edge.json, failing with a word fs names. */
export type Host = {
    secret(name: string): string | null;
    read(path: string, limit: number): string | Promise<string>;
    list(dir: string, limit: number): string[] | Promise<string[]>;
};

/* Every system module, each opened per package with the scopes that package holds, one per name MODULES lists. */
export const SYSTEM = { fs, net, secret, time } satisfies Record<Module, (pkg: string, held: string[], host: Host) => unknown>;
