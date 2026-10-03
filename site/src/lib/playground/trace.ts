import type { TraceEvent } from '../../../../js/src/system/trace'

type Node = { at: number; ms: number; pkg: string | null; name: string; detail: string; failed: string | null; children: Node[]; bytes?: number; prints?: number; parent?: Node; row?: HTMLDivElement }

// The calls that carry the id a request or a socket answered with, so they sit under it.
const FOLLOWS = new Set(['net.response', 'net.read', 'net.send', 'net.close'])

// The clock of the reader, in the zone their browser keeps.
const CLOCK = new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' })

// Rows past this are only counted, so a run of thousands stays quick to draw.
const ROWS = 5000

// Each bar keeps its own start and length and the list one total, so a longer run rescales them all at once.
const LEFT = 'min(calc(var(--at) / var(--end) * 100%), calc(100% - 2px))'
const WIDTH = 'calc(var(--ms) / var(--end) * 100%)'

// A div and not a list item, since prose spaces the items of a list and the lines must meet. Rows out of view skip their layout.
const ROW = 'grid min-h-6 grid-cols-[4rem_1fr_auto_2.5rem] sm:grid-cols-[4rem_1fr_auto_3.5rem] gap-3 [content-visibility:auto] [contain-intrinsic-size:auto_1.5rem]'

const lasted = (ms: number) => (ms < 1 ? '' : ms < 1000 ? `${Math.round(ms)}ms` : `${(ms / 1000).toFixed(2)}s`)

/* A size a reader sees beside a body, whole bytes until a kilobyte. */
const size = (bytes: number) => (bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(1)} KB`)

const leaf = (at: number, ms: number, pkg: string | null, name: string, detail: string, failed: string | null = null): Node => ({ at, ms, pkg, name, detail, failed, children: [] })

// A column of the guide, a line passing through, a branch to a sibling still to come, the last branch, or nothing.
type Cell = 'pass' | 'branch' | 'last' | 'blank'

/* The guide that joins a node to its parent, a line running on while a sibling is still to come. */
function guide(node: Node): Cell[] {
  const own = (child: Node): Cell => (child.parent!.children.at(-1) === child ? 'last' : 'branch')
  if (!node.parent) return []
  if (!node.parent.parent) return [own(node)]
  return [own(node.parent) === 'last' ? 'blank' : 'pass', own(node)]
}

const LINE = 'absolute border-fg-placeholder'

/* A span holding only text, since a print or a url is whatever the program made it. */
function el(className: string, text = ''): HTMLSpanElement {
  const span = document.createElement('span')
  span.className = className
  span.textContent = text
  return span
}

/* One column of the guide drawn in borders, so the lines meet across rows whatever the line height. A branch leaves the line on a curve as tall as its radius, so the straight part never runs over it. */
function cell(kind: Cell): HTMLSpanElement {
  const column = el('relative w-5 shrink-0 self-stretch')
  const part = (classes: string) => column.appendChild(el(`${LINE} ${classes}`))
  if (kind === 'pass' || kind === 'branch') part('inset-y-0 left-1/2 border-l')
  if (kind === 'last') part('left-1/2 top-0 h-[calc(50%-6px)] border-l')
  if (kind === 'branch' || kind === 'last') part('left-1/2 top-[calc(50%-6px)] h-1.5 w-1/2 rounded-bl-[6px] border-b border-l')
  return column
}

/* What one row shows, its time, its place in the tree, its bar and how long it took or how it failed. */
function parts(node: Node, epoch: number): HTMLSpanElement[] {
  // Cut at a fixed width, so one long print never pushes every bar out of view, and whole on hover.
  const detail = el('ml-2 max-w-[40ch] truncate text-fg-hint', node.detail)
  detail.title = node.detail
  const label = el('flex min-w-0 items-center')
  const name = node.prints ? `${node.name} ×${node.prints.toLocaleString('en')}` : node.name
  label.replaceChildren(...guide(node).map(cell), el(node.failed ? 'ml-1 shrink-0 text-danger' : 'ml-1 shrink-0', name), detail)
  if (node.pkg && node.pkg !== 'main') label.appendChild(el('ml-2 shrink-0 rounded-full border border-line-strong px-1.5 text-fg-subtle', node.pkg))

  // Narrower only once the box drops under the 24rem the rows keep, which is when it scrolls.
  const lane = el('relative h-1.5 w-16 self-center overflow-hidden rounded-full bg-line-strong @min-[24rem]:w-24')
  const bar = lane.appendChild(el(`absolute top-0 h-full min-w-0.5 rounded-full ${node.failed ? 'bg-danger' : node.name === 'print' ? 'bg-fg-muted' : 'bg-fg-hint'}`))
  bar.style.setProperty('--at', String(node.at))
  bar.style.setProperty('--ms', String(node.ms))
  bar.style.left = LEFT
  bar.style.width = WIDTH

  const took = el(node.failed ? 'self-center text-right text-danger' : 'self-center text-right tabular-nums text-fg-hint', node.failed ?? lasted(node.ms))
  return [el('self-center tabular-nums text-fg-hint', CLOCK.format(epoch + node.at)), label, lane, took]
}

/* The trace of the latest run, each event placed as it lands and only the rows it changed drawn again. */
export function createTrace(list: HTMLElement, entry = 'main.py') {
  let root = leaf(0, 0, null, 'run', entry)
  let opened = new Map<number, Node>()
  let reads = new Map<number, Node>()
  let queue: TraceEvent[] = []
  let total = 0
  let end = 0
  let frame = 0
  let shown = 0
  let hidden = 0
  // A host that sends no start of its own leaves the page clock to stand in.
  let epoch = Date.now()
  const fresh: Node[] = []
  const dirty = new Set<Node>()
  const more = el('block min-h-6 pl-[4.75rem] text-fg-hint')

  /* A node under its parent, its row placed after the rows already there, or only counted past the cap. */
  const attach = (node: Node, parent: Node) => {
    node.parent = parent
    const before = parent.children.at(-1)
    parent.children.push(node)
    // A sibling that was last no longer is, so it and what hangs under it draw their guide again.
    if (before) [before, ...before.children].forEach((each) => dirty.add(each))
    if (!parent.row || shown >= ROWS) return void hidden++
    shown++
    node.row = document.createElement('div')
    node.row.className = ROW
    // Under its request after what already answered it, so a late answer still sits beneath it.
    const after = parent === root ? null : (before?.row ?? parent.row)
    if (after) after.after(node.row)
    else list.insertBefore(node.row, more.isConnected ? more : null)
    fresh.push(node)
  }

  // A request lasts until the last thing that answered it, which is what its bar shows.
  const grow = (parent: Node, child: Node) => {
    parent.ms = Math.max(parent.ms, child.at + child.ms - parent.at)
    dirty.add(parent)
  }

  /* One event into the tree, a request with what answered it underneath and its body reads folded into one. */
  const add = (event: TraceEvent) => {
    end = Math.max(end, event.at + ('ms' in event ? event.ms : 0))
    if (event.kind === 'run') {
      epoch = event.epoch
      // Rows drawn before the start arrived read the page clock, so they draw again.
      const all = (node: Node): Node[] => [node, ...node.children.flatMap(all)]
      all(root).forEach((node) => dirty.add(node))
      return
    }
    if (event.kind === 'print') {
      const last = root.children.at(-1)
      const text = event.text.trimEnd()
      // Prints in a row read as one, counted, with the latest line beside it.
      if (last?.name === 'print') {
        last.prints = (last.prints ?? 1) + 1
        last.detail = text
        last.ms = event.at - last.at
        dirty.add(last)
        return
      }
      return attach(leaf(event.at, 0, null, 'print', text), root)
    }
    if (event.kind === 'sleep') return attach(leaf(event.at, event.ms, null, 'sleep', ''), root)

    const failed = event.outcome === 'ok' ? null : event.outcome
    const id = event.id
    const parent = id === undefined ? undefined : opened.get(id)

    if (parent && id !== undefined && FOLLOWS.has(event.call)) {
      if (event.call === 'net.read' && !failed) {
        // Every chunk of one body reads as a single row, its bytes added up.
        const folded = reads.get(id)
        if (folded) {
          folded.bytes = (folded.bytes ?? 0) + (event.bytes ?? 0)
          folded.ms = event.at + event.ms - folded.at
          folded.detail = size(folded.bytes)
          dirty.add(folded)
          return grow(parent, folded)
        }
        const read = { ...leaf(event.at, event.ms, event.pkg, 'read', size(event.bytes ?? 0), failed), bytes: event.bytes ?? 0 }
        reads.set(id, read)
        attach(read, parent)
        return grow(parent, read)
      }
      const answer = leaf(event.at, event.ms, event.pkg, event.call.split('.')[1]!, event.status === undefined ? '' : String(event.status), failed)
      attach(answer, parent)
      return grow(parent, answer)
    }

    const node = leaf(event.at, event.ms, event.pkg, event.call, event.scope, failed)
    if ((event.call === 'net.request' || event.call === 'net.connect') && id !== undefined) opened.set(id, node)
    attach(node, root)
  }

  // Every event that landed since the last frame, then only the rows they touched.
  const paint = () => {
    frame = 0
    queue.forEach(add)
    queue = []
    root.ms = total || end
    dirty.add(root)
    new Set([...fresh, ...dirty]).forEach((node) => node.row?.replaceChildren(...parts(node, epoch)))
    fresh.length = 0
    dirty.clear()
    list.style.setProperty('--end', String(Math.max(end, total) || 1))
    more.textContent = `+${hidden.toLocaleString('en')} more`
    if (hidden && !more.isConnected) list.append(more)
  }

  const later = () => { frame ||= requestAnimationFrame(paint) }

  // A new run starts from its root alone.
  const begin = () => {
    root = leaf(0, 0, null, 'run', entry)
    root.row = document.createElement('div')
    root.row.className = ROW
    opened = new Map()
    reads = new Map()
    queue = []
    total = 0
    end = 0
    shown = 0
    hidden = 0
    epoch = Date.now()
    fresh.length = 0
    dirty.clear()
    more.remove()
    list.replaceChildren(root.row)
  }

  begin()

  return {
    reset() {
      begin()
      later()
    },
    push(event: TraceEvent) {
      queue.push(event)
      later()
    },
    finish(ms: number) {
      total = ms
      later()
    }
  }
}
