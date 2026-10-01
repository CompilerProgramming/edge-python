// Every system module by name, the names no package and no import may take.
export const MODULES = ['net', 'secret', 'time'] as const;

export type Module = (typeof MODULES)[number];
