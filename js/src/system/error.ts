/* An error a system call raises in Python as the exception its name gives. */
export class SystemError extends Error {
    constructor(name: 'OSError' | 'PermissionError' | 'TimeoutError' | 'ValueError', message: string) {
        super(message);
        this.name = name;
    }
}
