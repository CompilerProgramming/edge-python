import compiler from '../../../../target/wasm32-unknown-unknown/release/compiler.wasm?module'
import { check } from '../../../../js/src/system/grants'
import { MODULES } from '../../../../js/src/system/names'
import { MAX_LOCK } from './packages'

const encode = new TextEncoder()
const decode = new TextDecoder()

type Exports = {
  memory: WebAssembly.Memory
  out_ptr(): number
  wasm_alloc(size: number): number
  wasm_free(ptr: number, size: number): void
  manifest_check(manifest: number, manifestLen: number, lock: number, lockLen: number, system: number, systemLen: number): number
  bundle_index(bundle: number, bundleLen: number): number
}

// One instance per isolate, since the rules it holds never change while the Worker is up.
let instance: Exports | null = null

/* The compiler as the library of rules every host shares, its host imports never called since no program runs here. */
function engine(): Exports {
  if (instance) return instance
  const imports: Record<string, Record<string, () => number>> = {}
  for (const { module, name } of WebAssembly.Module.imports(compiler)) (imports[module] ??= {})[name] = () => 0
  instance = new WebAssembly.Instance(compiler, imports).exports as unknown as Exports
  return instance
}

/* Stages each buffer in the compiler's memory for one export, frees them after, and reads the text it left. */
function call(buffers: Uint8Array[], run: (e: Exports, at: number[]) => number): string {
  const e = engine()
  const at = buffers.map((bytes) => {
    const ptr = e.wasm_alloc(Math.max(1, bytes.length))
    new Uint8Array(e.memory.buffer, ptr, bytes.length).set(bytes)
    return ptr
  })

  try {
    const len = run(e, at)
    return decode.decode(new Uint8Array(e.memory.buffer, e.out_ptr(), len))
  } finally {
    buffers.forEach((bytes, i) => e.wasm_free(at[i]!, Math.max(1, bytes.length)))
  }
}

/* Why the manifest a package carries is turned away, held with the lock beside it to the rules the CLI packs under, null when it holds. */
export function checkPackage(manifest: string, lock: string | undefined): string | null {
  const [m, l, s] = [manifest, lock ?? '', MODULES.join('\n')].map((text) => encode.encode(text))
  const problem = call([m!, l!, s!], (e, [mp, lp, sp]) => e.manifest_check(mp!, m!.length, lp!, l!.length, sp!, s!.length))
  return problem || null
}

/* Every file of a bundle by its path, cut from the artifact where the engine's decoder found it, and why when it is not a bundle. */
export function unpack(artifact: Uint8Array): Map<string, Uint8Array> {
  const answer = JSON.parse(call([artifact], (e, [at]) => e.bundle_index(at!, artifact.length))) as { error?: string; files?: [string, number, number][] }
  if (answer.error !== undefined) throw new Error(answer.error)
  return new Map((answer.files ?? []).map(([path, at, len]) => [path, artifact.subarray(at, at + len)]))
}

/* Holds each manifest a bundle carries to the engine's rules, its lock and its grants, here since Node cannot load the compiler. */
export function checkManifests(manifests: Record<string, string>, locks: Record<string, string>) {
  for (const [dir, source] of Object.entries(manifests)) {
    const at = `edge.json at '${dir}edge.json'`
    const beside = locks[dir]
    if (beside !== undefined && beside.length > MAX_LOCK) throw new Error(`A lock is ${MAX_LOCK} bytes at most.`)

    const problem = checkPackage(source, beside)
    if (problem) throw new Error(`${at}: ${problem}`)

    // The engine read the shape of the grants, the system modules say which entries exist.
    const grants = check((JSON.parse(source) as { permissions?: unknown }).permissions)
    if (grants) throw new Error(`${at}: ${grants}`)
  }
}
