import net from './net.ts';
import time from './time.ts';
import type { Module } from './names.ts';

/* Every system module, each opened per package with the scopes that package holds, one per name MODULES lists. */
export const SYSTEM = { net, time } satisfies Record<Module, (pkg: string, held: string[]) => unknown>;
