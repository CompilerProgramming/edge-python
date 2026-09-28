import * as engine from './engine.ts';
import { errMsg } from '../util.ts';
import type { WorkerRequest, WorkerMessage } from '../protocol.ts';

const post = (msg: WorkerMessage) => self.postMessage(msg);
const onLine = (text: string) => post({ type: 'line', text });

/* Fire-and-forget messages return this instead of a result, no 'response' is posted for them. */
const NO_REPLY: unique symbol = Symbol('no-reply');

// The program's files the page is reading, each settled when its answer arrives.
const reads = new Map<number, { resolve: (res: Response) => void, reject: (e: Error) => void }>();
let nextRead = 1;

/* Asks the page for a file of the program, since the room reaches nothing of the page's origin. */
const readFile = (url: string): Promise<Response> => new Promise((resolve, reject) => {
    const id = nextRead++;
    reads.set(id, { resolve, reject });
    post({ type: 'read', id, url });
});

function dispatch(req: WorkerRequest): unknown {
    switch (req.type) {
        case 'load': return engine.load(req.opts, readFile);
        case 'run': return engine.run({ src: req.src, repl: req.repl, entryDir: req.entryDir, incremental: req.incremental, input: req.input }, onLine);
        case 'set-preempt-interval': return engine.setPreemptInterval(req.interval);
        case 'pause': return engine.pause();
        case 'resume': return engine.resume();
        case 'save-state': return engine.saveState();
        case 'restore-state': return engine.restoreState({ blob: req.blob, onLine });
        case 'state-globals': return engine.stateGlobals();
        case 'state-stack': return engine.stateStack();
        case 'reset': return engine.reset();
        case 'clear-cache': return engine.clearCache();
        case 'dispose': engine.dispose(); self.close(); return NO_REPLY;
        // Wake a paused `receive()` in the running script.
        case 'push-event': engine.pushEvent(req.message); return NO_REPLY;
        case 'file': {
            const waiting = reads.get(req.id);
            reads.delete(req.id);
            if (req.status === 0) waiting?.reject(new TypeError('the page could not read it'));
            else waiting?.resolve(new Response(req.body, { status: req.status, headers: { 'content-type': req.contentType } }));
            return NO_REPLY;
        }
        default: {
            // Unreachable per the types, reached only on main/worker version drift.
            const _exhaustive: never = req;
            throw new Error(`unknown message type: ${JSON.stringify(_exhaustive)}`);
        }
    }
}

/* Web Worker entry, receives postMessage requests from `createWorker`, dispatches to the engine, posts responses. */
self.onmessage = async ({ data }: MessageEvent<WorkerRequest>) => {
    try {
        const result = await dispatch(data);
        if (result === NO_REPLY) return;
        post({ type: 'response', reqId: data.reqId, result });
    } catch (e) {
        post({ type: 'error', reqId: data.reqId, message: errMsg(e) });
    }
};
