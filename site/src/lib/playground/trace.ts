import type { TraceEvent } from '../../../../js/src/system/trace'

type Node = { at: number; ms: number; pkg: string | null; name: string; detail: string; failed: string | null; children: Node[]; bytes?: number }

// The calls that carry the id a request or a socket answered with, so they sit under it.
const FOLLOWS = new Set(['net.response', 'net.read', 'net.send', 'net.close'])

// The clock of the reader, in the zone their browser keeps.
const CLOCK = new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' })

const lasted = (ms: number) => (ms < 1 ? '' : ms < 1000 ? `${Math.round(ms)}ms` : `${(ms / 1000).toFixed(2)}s`)

/* A size a reader sees beside a body, whole bytes until a kilobyte. */
const size = (bytes: number) => (bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(1)} KB`)

const leaf = (at: number, ms: number, pkg: string | null, name: string, detail: string, failed: string | null = null): Node => ({ at, ms, pkg, name, detail, failed, children: [] })

/* The tree a run draws, a request with what answered it underneath and its body reads folded into one. */
function tree(events: TraceEvent[], entry: string, total: number): Node {
  const root = leaf(0, total, null, 'run', entry)
  const opened = new Map<number, Node>()
  const reads = new Map<number, Node>()

  for (const event of events) {
    if (event.kind === 'run') continue
    if (event.kind === 'print') {
      root.children.push(leaf(event.at, 0, null, 'print', event.text.trimEnd()))
      continue
    }
    if (event.kind === 'sleep') {
      root.children.push(leaf(event.at, event.ms, null, 'sleep', ''))
      continue
    }

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
          continue
        }
        const read = { ...leaf(event.at, event.ms, event.pkg, 'read', '', failed), bytes: event.bytes ?? 0 }
        reads.set(id, read)
        parent.children.push(read)
        continue
      }
      parent.children.push(leaf(event.at, event.ms, event.pkg, event.call.split('.')[1]!, event.status === undefined ? '' : String(event.status), failed))
      continue
    }

    const node = leaf(event.at, event.ms, event.pkg, event.call, event.scope, failed)
    if ((event.call === 'net.request' || event.call === 'net.connect') && id !== undefined) opened.set(id, node)
    root.children.push(node)
  }

  // A request lasts until the last thing that answered it, which is what its bar shows.
  for (const node of root.children) {
    for (const child of node.children) {
      node.ms = Math.max(node.ms, child.at + child.ms - node.at)
      if (child.bytes !== undefined) child.detail = size(child.bytes)
    }
  }

  return root
}

// A column of the guide, a line passing through, a branch to a sibling still to come, the last branch, or nothing.
type Cell = 'pass' | 'branch' | 'last' | 'blank'

/* Each node beside the guide that joins it to its parent, a line running on while a sibling is still to come. */
function lines(node: Node, lead: Cell[] = [], last = true, top = true, out: { node: Node; guide: Cell[] }[] = []) {
  out.push({ node, guide: top ? [] : [...lead, last ? 'last' : 'branch'] })
  const next: Cell[] = top ? [] : [...lead, last ? 'blank' : 'pass']
  node.children.forEach((child, at) => lines(child, next, at === node.children.length - 1, false, out))
  return out
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

/* One row, its time, its place in the tree, its bar and how long it took or how it failed. */
function draw(node: Node, guide: Cell[], epoch: number, total: number): HTMLDivElement {
  // A div and not a list item, since prose spaces the items of a list and the lines must meet.
  const item = document.createElement('div')
  item.className = 'grid min-h-6 grid-cols-[4rem_1fr_auto_2.5rem] sm:grid-cols-[4rem_1fr_auto_3.5rem] gap-3'
  const share = (ms: number) => (total ? (ms / total) * 100 : 0)

  const label = el('flex min-w-0 items-center')
  label.replaceChildren(...guide.map(cell), el(node.failed ? 'ml-1 shrink-0 text-danger' : 'ml-1 shrink-0', node.name), el('ml-2 whitespace-nowrap text-fg-hint', node.detail))
  if (node.pkg && node.pkg !== 'main') label.appendChild(el('ml-2 shrink-0 rounded-full border border-line-strong px-1.5 text-fg-subtle', node.pkg))

  // Narrower only once the box drops under the 24rem the rows keep, which is when it scrolls.
  const lane = el('relative h-1.5 w-16 self-center overflow-hidden rounded-full bg-line-strong @min-[24rem]:w-24')
  const bar = lane.appendChild(el(`absolute top-0 h-full min-w-0.5 rounded-full ${node.failed ? 'bg-danger' : node.name === 'print' ? 'bg-fg-muted' : 'bg-fg-hint'}`))
  // Held inside the track, so a tick at the very end still shows.
  bar.style.left = `min(${share(node.at)}%, calc(100% - 2px))`
  bar.style.width = `${share(node.ms)}%`

  const took = el(node.failed ? 'self-center text-right text-danger' : 'self-center text-right tabular-nums text-fg-hint', node.failed ?? lasted(node.ms))
  item.replaceChildren(el('self-center tabular-nums text-fg-hint', CLOCK.format(epoch + node.at)), label, lane, took)
  return item
}

/* The trace of the latest run, drawn again as each event lands and once more when the run ends with its total. */
export function createTrace(list: HTMLElement, entry = 'main.py') {
  let events: TraceEvent[] = []
  let total = 0
  let frame = 0
  // A host that sends no start of its own leaves the page clock to stand in.
  let began = Date.now()

  const paint = () => {
    frame = 0
    const epoch = events.find((event): event is Extract<TraceEvent, { kind: 'run' }> => event.kind === 'run')?.epoch ?? began
    const end = events.reduce((last, event) => Math.max(last, event.at + ('ms' in event ? event.ms : 0)), total)
    list.replaceChildren(...lines(tree(events, entry, total || end)).map(({ node, guide }) => draw(node, guide, epoch, end)))
  }

  const later = () => { frame ||= requestAnimationFrame(paint) }

  return {
    reset() {
      events = []
      total = 0
      began = Date.now()
      later()
    },
    push(event: TraceEvent) {
      events.push(event)
      later()
    },
    finish(ms: number) {
      total = ms
      later()
    }
  }
}
