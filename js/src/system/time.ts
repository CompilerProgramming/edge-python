import type { EdgeValue } from '../rt.ts';
import { SystemError } from './error.ts';

/* Nanoseconds since the Unix epoch, or since an arbitrary start that never goes back for 'monotonic'. */
function now(clock: EdgeValue = 'wall'): bigint {
    if (clock === 'wall') return BigInt(Date.now()) * 1_000_000n;
    if (clock === 'monotonic') return BigInt(Math.round(performance.now() * 1e6));
    throw new SystemError('ValueError', `time.now takes 'wall' or 'monotonic', not ${JSON.stringify(clock)}`);
}

/* The IANA zone the host runs in and its offset from UTC in seconds. */
function zone(): [string, number] {
    return [Intl.DateTimeFormat().resolvedOptions().timeZone, -new Date().getTimezoneOffset() * 60];
}

export default { now, zone };
