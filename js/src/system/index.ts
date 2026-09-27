import net from './net.ts';
import time from './time.ts';

/* Every system module, each opened per package with the scopes that package holds. */
export const SYSTEM = { net, time };
