import type { EdgeValue } from '../rt.ts';
import { batched } from './batch.ts';
import { SystemError } from './error.ts';
import { need, plainPath } from './grants.ts';
import type { Host } from './index.ts';

// The most files one list hands over and the largest file one read does.
const FILES = 10_000;
const BYTES = 10 * 1024 * 1024;

// Each host fails with one of these words, so both raise the same error for the same file.
const WHY: Record<string, (at: string) => string> = {
    missing: (at) => `the project has no file or folder '${at || '.'}'`,
    large: (at) => `'${at}' is larger than ${BYTES} bytes`,
    binary: (at) => `'${at}' is not UTF-8 text`,
    none: () => 'this host keeps no project files to read',
};

const failed = (e: unknown, at: string) => new SystemError('OSError', (WHY[(e as Error)?.message] ?? (() => `reading '${at}' failed`))(at));

/* What a host answers, now or once it settles, with its failure raised as the OSError both hosts share. */
function settle<T, R>(ask: () => T | Promise<T>, at: string, done: (value: T) => R): R | Promise<R> {
    let answer: T | Promise<T>;
    try {
        answer = ask();
    } catch (e) {
        throw failed(e, at);
    }
    return answer instanceof Promise ? answer.then(done, (e) => { throw failed(e, at); }) : done(answer);
}

/* The fs calls of one package, each reading only under the folders it holds, from wherever its host keeps the project. */
export default function fs(pkg: string, held: string[], host: Host) {
    // A path that could leave the project is refused like one outside the grant, since both reach past it.
    const path = (value: EdgeValue, call: string): string => {
        if (typeof value !== 'string') throw new SystemError('ValueError', `${call} takes a path as a str`);
        const found = plainPath(value);
        if (found === null) throw new SystemError('PermissionError', `'${pkg}' cannot reach '${value}', fs reads plain paths under the project, with no '..', no leading '/' and no name starting with a dot`);
        return found;
    };

    // A held folder reaches what lies under it, and the root reaches every path.
    const within = (at: string) => {
        if (held.map(plainPath).some((dir) => dir === '' || (dir !== null && (at === dir || at.startsWith(`${dir}/`))))) return;
        need(pkg, 'fs', held, at === '' ? '.' : `./${at}`);
    };

    /* The text of one file, read as UTF-8. */
    function read(file: EdgeValue): string | Promise<string> {
        const at = path(file, 'fs.read');
        if (at === '') throw new SystemError('ValueError', 'fs.read takes a file, not the project folder');
        within(at);
        return settle(() => host.read(at, BYTES), at, (text) => text);
    }

    /* Every file under a folder, its path from the root edge.json, in order. */
    function list(dir: EdgeValue = '.'): string[] | Promise<string[]> {
        const at = path(dir, 'fs.list');
        within(at);
        return settle(() => host.list(at, FILES + 1), at, (found) => {
            if (found.length > FILES) throw new SystemError('OSError', `'${at || '.'}' holds more than ${FILES} files`);
            return [...found].sort();
        });
    }

    const calls = { read, list };
    return { calls: { ...calls, batch: batched('fs', calls) }, close() {} };
}
