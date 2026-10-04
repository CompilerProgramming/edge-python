/* Shapes crossing the main-thread/worker postMessage boundary. Plain data only, no DOM or WebWorker globals, so both the `dom` and `webworker` lib scopes can import it without mixing libs in one program. */

import type { Permissions } from './system/grants.ts';
import type { TraceEvent } from './system/trace.ts';

/* Caps a run boots under, memory in MB, a field left out keeps the sandbox value of the engine. */
export interface Limits {
    memory?: number
    ops?: number
}

export interface LoadOpts {
    wasmUrl?: string
    // The compiler's bytes, which a room receives from its page instead of fetching them.
    wasm?: ArrayBuffer | null
    imports?: Record<string, string> | null
    // What the embedder's root manifest grants, beside the imports it declares.
    permissions?: Permissions | null
    // What secret reads, each value only under a name the permissions grant.
    secrets?: Record<string, string> | null
    // Whether each run reports what it reached, which only an embedder that listens pays for.
    trace?: boolean | null
    // The program's directory, where its files and its edge.json live.
    baseUrl?: string | null
    limits?: Limits | null
}

export interface RunOpts {
    src: string
    repl?: boolean
    // The script `src` came from, its relative imports resolve beside it, the project root when absent.
    entry?: string
    incremental?: boolean
    input?: string
}

export interface ExecResult {
    out: string
    ms: number
    exitCode?: number
}

/* Requests main to worker. `reqId` correlates each 'response'/'error' answer, fire-and-forget types omit it. */
export type WorkerRequest =
    | { type: 'load', reqId: number, opts: LoadOpts }
    | ({ type: 'run', reqId: number } & RunOpts)
    | { type: 'set-preempt-interval', reqId: number, interval: number }
    | { type: 'pause', reqId: number }
    | { type: 'resume', reqId: number }
    | { type: 'save-state', reqId: number }
    | { type: 'restore-state', reqId: number, blob: Uint8Array | ArrayBuffer }
    | { type: 'state-globals', reqId: number }
    | { type: 'state-stack', reqId: number }
    | { type: 'reset', reqId: number }
    | { type: 'clear-cache', reqId: number }
    | { type: 'push-event', reqId?: number, message: string }
    | { type: 'dispose', reqId?: number }
    // A file the page read for the room, status 0 when it refused or could not read it.
    | { type: 'file', reqId?: number, id: number, status: number, contentType: string, body: ArrayBuffer | null };

/* Pushes worker to main. 'response' answers a request's reqId, the rest are unsolicited. */
export type WorkerMessage =
    | { type: 'line', text: string }
    | { type: 'trace', event: TraceEvent }
    | { type: 'read', id: number, url: string }
    | { type: 'response', reqId?: number, result: unknown }
    | { type: 'error', reqId?: number, message: string };
