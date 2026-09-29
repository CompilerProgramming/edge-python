import { unpack } from './engine'

// The prefix the CLI packs documentation under, which no import can reach.
const DOCS = '@docs/'

// A manifest and the lock that resolves what it declares, one pair per directory.
const MANIFEST = 'edge.json'
const LOCK = 'edge.lock'

const text = new TextDecoder()

/* Everything the registry needs about a release, read out of the artifact rather than taken on the publisher's word. The values stay unknown because naming them is not the same as vouching for them, the route still validates every one. */
export type Packed = {
  name: unknown
  version: unknown
  description: unknown
  repository: unknown
  edge: unknown
  notice: string | null
  docs: Record<string, string>
  // Each manifest the bundle carries beside the lock that resolves it, both as written.
  manifests: Record<string, string>
  locks: Record<string, string>
}

/* What the bundle says about itself. A field the artifact does not carry comes back null, so an old CLI publishes with less rather than failing. */
export function packed(artifact: Uint8Array): Packed {
  // The engine's decoder, the one every host reads a package with, caps and path rules included.
  const files = unpack(artifact)

  const declared = files.get('edge.json')
  if (!declared) throw new Error('That package carries no edge.json, so it has nothing to publish under.')

  let manifest: Record<string, unknown>
  try {
    manifest = JSON.parse(text.decode(declared))
  } catch {
    throw new Error('The edge.json inside that package is not JSON.')
  }

  if (manifest == null || typeof manifest !== 'object' || Array.isArray(manifest)) {
    throw new Error('The edge.json inside that package is not an object.')
  }

  return {
    name: manifest.name,
    version: manifest.version,
    description: manifest.description ?? null,
    repository: manifest.repository ?? null,
    edge: manifest.edge ?? null,
    notice: notice(files),
    docs: docs(files),
    manifests: named(files, MANIFEST),
    locks: named(files, LOCK)
  }
}

/* Every file with this name whatever directory it sits in, keyed by that directory, so a nested package answers for itself. */
function named(files: Map<string, Uint8Array>, file: string): Record<string, string> {
  const found: Record<string, string> = {}

  for (const [path, bytes] of files) {
    if (path === file || path.endsWith(`/${file}`)) found[path.slice(0, -file.length)] = text.decode(bytes)
  }

  return found
}

/* The LICENSE at the root whatever its extension, which the registry reads to name the license instead of believing a name. */
function notice(files: Map<string, Uint8Array>): string | null {
  for (const [path, bytes] of files) {
    if (path.includes('/')) continue
    if (path.split('.')[0]?.toUpperCase() === 'LICENSE') return text.decode(bytes)
  }

  return null
}

/* The pages the bundle carries, keyed by the path the site orders them with. */
function docs(files: Map<string, Uint8Array>): Record<string, string> {
  const found: Record<string, string> = {}

  for (const [path, bytes] of files) {
    if (path.startsWith(DOCS)) found[path.slice(DOCS.length)] = text.decode(bytes)
  }

  return found
}
