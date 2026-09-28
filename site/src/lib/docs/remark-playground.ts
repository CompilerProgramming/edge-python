// Collapses a runnable fence, the ```edge-manifest right before it and the ```output right after it into one embedded editor.
const RUNNABLE = 'edge-python'
const MANIFEST = 'edge-manifest'
const OUTPUT = 'output'

type Node = { type: string; lang?: string | null; value?: string }
type Root = { children: Node[] }
type File = { data: { astro?: { frontmatter?: Record<string, unknown> } } }

// The edge.json an example runs under, its code and what it prints.
export type Example = { manifest: string; code: string; output: string }

const embed = (example: Example) => ({
  type: 'mdxJsxFlowElement',
  name: 'Playground',
  attributes: [
    { type: 'mdxJsxAttribute', name: 'manifest', value: example.manifest },
    { type: 'mdxJsxAttribute', name: 'code', value: example.code },
    { type: 'mdxJsxAttribute', name: 'output', value: example.output }
  ],
  children: []
})

// Where an example sat, for a renderer that gets HTML back as a string and places the editor itself.
export const MARK = /<!--playground:(\d+)-->/
const mark = (at: number) => ({ type: 'html', value: `<!--playground:${at}-->` })

// The frontmatter key the collected examples come back under, since that is how a plugin answers a render.
export const EXAMPLES = 'playgrounds'

/* Collecting, the examples come back through the frontmatter and a marker holds each place, which is how a page rendered at request time reaches the same component. Otherwise the example becomes the MDX element the build compiles. */
export function remarkPlayground(collect = false) {
  return (tree: Root, file: File) => {
    const found: Example[] = []

    for (let i = tree.children.length - 1; i >= 0; i--) {
      const node = tree.children[i]
      if (node?.type !== 'code' || node.lang !== RUNNABLE) continue

      const before = tree.children[i - 1]
      const next = tree.children[i + 1]
      const declared = before?.type === 'code' && before.lang === MANIFEST
      const paired = next?.type === 'code' && next.lang === OUTPUT
      // An example that declares no manifest runs under an empty one.
      const example = { manifest: (declared && before?.value) || '{}', code: node.value ?? '', output: paired ? (next?.value ?? '') : '' }

      const start = declared ? i - 1 : i
      tree.children.splice(start, 1 + Number(declared) + Number(paired), collect ? mark(found.push(example) - 1) : embed(example))
      i = start
    }

    if (!collect) return

    const astro = (file.data.astro ??= {})
    astro.frontmatter = { ...astro.frontmatter, [EXAMPLES]: found }
  }
}
