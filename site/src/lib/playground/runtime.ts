const TIMEOUT_MS = 10000
const LOAD_MS = 7000

export type Phase = 'runtime' | 'worker' | 'running'

// The edge.json an example runs under, its imports and its grants reach the room.
export type Manifest = { imports?: Record<string, string>; permissions?: Record<string, string[]> }

type Worker = {
  run(source: string): Promise<{ out: string; ms: number }>
  onOutput(handler: (chunk: string) => void): void
  dispose(): void
}

// One room per manifest, since a room's grants and the hosts it reaches are fixed once it opens.
const rooms = new Map<string, Promise<Worker>>()
const ready = new Set<string>()
let sink: ((chunk: string) => void) | null = null
let queue: Promise<unknown> = Promise.resolve()

async function kill(key: string) {
  const pending = rooms.get(key)
  rooms.delete(key)
  ready.delete(key)
  sink = null

  try {
    ;(await pending)?.dispose()
  } catch {}
}

// The JS host and the compiler come from this environment's CDN, set in wrangler.json.
function spawn(cdn: string, key: string, manifest: Manifest, onPhase?: (phase: Phase) => void): Promise<Worker> {
  const open = rooms.get(key)
  if (open) return open

  const load = async () => {
    onPhase?.('runtime')
    const { createWorker } = await import(/* @vite-ignore */ `${cdn}/js/src/index.js`)

    onPhase?.('worker')
    const spawned: Worker = await createWorker({ wasmUrl: `${cdn}/compiler.wasm`, imports: manifest.imports ?? {}, permissions: manifest.permissions ?? {} })
    spawned.onOutput((chunk) => sink?.(chunk))
    ready.add(key)

    return spawned
  }

  const silence = new Promise<never>((_, reject) => {
    setTimeout(() => reject(new Error(`No response after ${LOAD_MS / 1000}s`)), LOAD_MS)
  })

  // A room that never opened is forgotten, so the next run tries again.
  const worker = Promise.race([load(), silence]).catch((error) => {
    console.error(error)
    rooms.delete(key)
    throw new Error("Couldn't load the runtime. Check your connection and try again.")
  })

  rooms.set(key, worker)
  return worker
}

export async function run(
  source: string,
  cdn: string,
  manifest: Manifest,
  onChunk: (chunk: string) => void,
  onPhase?: (phase: Phase) => void
): Promise<{ error: string; ms: number }> {
  // Two examples whose manifests read the same share one room.
  const key = JSON.stringify(manifest)

  const exec = async () => {
    const active = await spawn(cdn, key, manifest, ready.has(key) ? undefined : onPhase)
    onPhase?.('running')
    sink = onChunk

    let timer: ReturnType<typeof setTimeout> | undefined

    try {
      const running = active.run(source)
      running.catch(() => {})

      const timeout = new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`Run exceeded ${TIMEOUT_MS / 1000}s, worker terminated`)),
          TIMEOUT_MS
        )
      })

      const { out, ms } = await Promise.race([running, timeout])
      return { error: out || '', ms }
    } catch (error) {
      await kill(key)
      throw error
    } finally {
      clearTimeout(timer)
      sink = null
    }
  }

  const result = queue.then(exec, exec)
  queue = result.catch(() => {})

  return result
}
