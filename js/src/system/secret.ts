import type { EdgeValue } from '../rt.ts';
import { batched } from './batch.ts';
import { SystemError } from './error.ts';
import { need } from './grants.ts';
import type { Host } from './index.ts';

/* The secret calls of one package, each reading from the host only a name the package holds. */
export default function secret(pkg: string, held: string[], host: Host) {
    /* The value the host keeps under `name`, asked for only once the grant allows it. */
    function read(name: EdgeValue): string {
        if (typeof name !== 'string') throw new SystemError('ValueError', 'secret.read takes a name as a str');
        need(pkg, 'secret', held, name);
        const value = host.secret(name);
        if (value === null) throw new SystemError('OSError', `the host holds no value for ${name}`);
        return value;
    }

    const calls = { read };
    return { calls: { ...calls, batch: batched('secret', calls) }, close() {} };
}
