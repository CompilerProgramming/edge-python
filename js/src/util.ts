import type { CompilerExports } from './wasm.ts';
import { SystemError } from './system/error.ts';

export const errMsg = (e: unknown): string => e instanceof Error ? e.message : String(e);

// The error kinds of abi/src/lib.rs a host raises through, a custom one carries its class in the message.
export const ERR_TYPE = 0;
export const ERR_RUNTIME = 2;
export const ERR_CUSTOM = 6;

/* The kind and message a failed system call raises with, its own class for a SystemError. */
export const fault = (e: unknown): [number, string] => e instanceof SystemError ? [ERR_CUSTOM, `${e.name}: ${e.message}`] : [ERR_RUNTIME, errMsg(e)];

export const writeBytes = (exports: CompilerExports, bytes: Uint8Array): number => {
    const ptr = exports.wasm_alloc(Math.max(1, bytes.length));
    new Uint8Array(exports.memory.buffer, ptr, bytes.length).set(bytes);
    return ptr;
};
