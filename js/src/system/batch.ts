import type { EdgeValue } from '../rt.ts';
import { SystemError } from './error.ts';

type Calls = Record<string, (...args: EdgeValue[]) => unknown>;

/* Many calls of one module in a single crossing, each `[name, *args]`, their answers in order. */
export function batched(module: string, calls: Calls): (list: EdgeValue) => unknown {
    return (list) => {
        const shape = `${module}.batch takes a list of [call, *args] lists`;
        if (!Array.isArray(list)) throw new SystemError('ValueError', shape);
        // Every entry is checked before any runs, so a malformed batch starts nothing.
        const steps = list.map((entry) => {
            if (!Array.isArray(entry) || typeof entry[0] !== 'string') throw new SystemError('ValueError', shape);
            const [name, ...args] = entry;
            const call = Object.hasOwn(calls, name) ? calls[name] : undefined;
            if (!call) throw new SystemError('ValueError', `${module} has no call '${name}'`);
            return () => call(...args);
        });
        const answers: unknown[] = [];
        try {
            for (const step of steps) answers.push(step());
        } catch (e) {
            // What already started still settles, unobserved, rather than surfacing as unhandled.
            for (const answer of answers) if (answer instanceof Promise) answer.catch(() => {});
            throw e;
        }
        // A call that waits holds the whole batch, which answers once every one settles.
        return answers.some((answer) => answer instanceof Promise) ? Promise.all(answers) : answers;
    };
}
