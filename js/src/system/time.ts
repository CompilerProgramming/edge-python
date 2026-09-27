import type { EdgeValue } from '../rt.ts';
import { SystemError } from './error.ts';
import { need } from './grants.ts';

/* The time calls of one package, each checked against the clocks it holds. */
export default function time(pkg: string, held: string[]) {
    /* Nanoseconds since the Unix epoch, or since an arbitrary start that never goes back for 'monotonic'. */
    function now(clock: EdgeValue = 'wall'): bigint {
        if (clock !== 'wall' && clock !== 'monotonic') throw new SystemError('ValueError', `time.now takes 'wall' or 'monotonic', not ${JSON.stringify(clock)}`);
        need(pkg, 'time', held, clock);
        return clock === 'wall' ? BigInt(Date.now()) * 1_000_000n : BigInt(Math.round(performance.now() * 1e6));
    }

    /* The IANA zone the host runs in and its offset from UTC in seconds. */
    function zone(): [string, number] {
        need(pkg, 'time', held, 'zone');
        return [Intl.DateTimeFormat().resolvedOptions().timeZone, -new Date().getTimezoneOffset() * 60];
    }

    return { calls: { now, zone }, close() {} };
}
