// The layout rules a page has to follow, mirrored in cli/src/docs.rs and locked by tests/cases/docs.json.
export const ORDER = /^\d+[-_]/

/* Refuses a page the renderer cannot lay out, and hands back what it read on the way, so a caller that needs the title does not parse the frontmatter a second time. */
export function check(page: string, text: string) {
  const segments = page.split('/')
  if (segments.length > 2) {
    throw new Error(`'${page}' nests deeper than one folder, a section and its pages is all the renderer orders`)
  }
  if (!segments.every((segment) => ORDER.test(segment))) {
    throw new Error(`'${page}' needs a numeric prefix on every segment, like '01-reference/02-cli.mdx'`)
  }

  const read = front(page, text)

  let open: string | null = null
  let closed: string | null = null
  let headings = 0
  // The lines of the edge-manifest being read, checked once its fence closes.
  let manifest: string[] = []
  const lone = () => new Error(`'${page}' has an edge-manifest block that no edge-python block follows`)

  for (const line of read.body.split('\n')) {
    const trimmed = line.trim()
    if (trimmed.startsWith('```')) {
      if (open !== null) {
        if (open === 'edge-manifest' && !object(manifest.join('\n'))) {
          throw new Error(`'${page}' has an edge-manifest block that is not a JSON object`)
        }
        closed = open
        open = null
      } else {
        const lang = trimmed.slice(3).trim()
        if (lang === 'output' && closed !== 'edge-python') {
          throw new Error(`'${page}' has an output block that follows no edge-python block`)
        }
        if (closed === 'edge-manifest' && lang !== 'edge-python') throw lone()
        open = lang
        closed = null
        manifest = []
      }
      continue
    }
    if (open === 'edge-manifest') manifest.push(line)
    if (open === null) {
      if (line.startsWith('# ')) headings++
      if (tagged(line)) {
        throw new Error(`'${page}' writes the raw HTML '${tagged(line)}', and a page is markdown the site renders itself`)
      }
      // Blank lines keep two fences adjacent, prose between them does not.
      if (trimmed) {
        if (closed === 'edge-manifest') throw lone()
        closed = null
      }
    }
  }

  if (open !== null) throw new Error(`'${page}' leaves a code fence unterminated`)
  if (closed === 'edge-manifest') throw lone()
  if (headings !== 1) throw new Error(`'${page}' has ${headings} top-level headings, the renderer needs exactly one`)

  return read
}

// An edge-manifest holds one JSON object, the edge.json its example runs under.
function object(text: string): boolean {
  try {
    const value: unknown = JSON.parse(text)
    return typeof value === 'object' && value !== null && !Array.isArray(value)
  } catch {
    return false
  }
}

/* The first HTML tag a prose line opens, since a page the registry renders is markdown from a stranger and a raw tag would run on our origin. Inline code drops out first, so a page can still write about `<script>`. */
function tagged(line: string): string | null {
  const prose = line.replace(/`[^`]*`/g, '')
  // A tag that closes right after its name reads as one, a tag with attributes shows only its name.
  return prose.match(/<\/?[A-Za-z][^\s>]*>?/)?.[0] ?? null
}

/* A page's frontmatter and the body past it, the same walk that refuses a page the renderer cannot lay out. The keys come back because a page is read for its title long after it was checked for one. */
export function front(page: string, text: string): { keys: Map<string, string>; body: string } {
  const start = text.startsWith('---\n') ? 4 : text.startsWith('---\r\n') ? 5 : -1
  if (start < 0) {
    throw new Error(`'${page}' opens with no frontmatter, a page needs a title and a description`)
  }

  const rest = text.slice(start)
  const keys = new Map<string, string>()
  let at = 0

  for (const line of rest.split('\n')) {
    const trimmed = line.trimEnd()
    if (trimmed === '---') {
      if (!keys.has('title') || !keys.has('description')) {
        throw new Error(`'${page}' needs both a title and a description in its frontmatter`)
      }
      return { keys, body: rest.slice(at + line.length + 1) }
    }
    if (trimmed) {
      const colon = trimmed.indexOf(':')
      if (colon < 0) {
        throw new Error(`'${page}' has the frontmatter line '${trimmed}', which is no key and value`)
      }
      const key = trimmed.slice(0, colon).trim()
      const value = trimmed.slice(colon + 1).trim()
      if (!value) {
        throw new Error(`'${page}' leaves the frontmatter '${key}' empty`)
      }
      keys.set(key, value.replace(/^["']|["']$/g, ''))
    }
    at += line.length + 1
  }
  throw new Error(`'${page}' leaves its frontmatter unterminated`)
}
