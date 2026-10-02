import type { ComponentProps } from 'astro/types'
import type Icon from '../components/ui/icon.astro'
import { OWNER } from '../lib/account/handle'

// What the gallery shows, newest first, each program a page of its own under /gallery.
export type Program = { slug: string; name: string; description: string; icon: ComponentProps<typeof Icon>['name']; author: string }

export const gallery: Program[] = [
  { slug: 'rails-cracker', name: 'Rails cracker', description: 'Paste a GitHub repo and follow its code graph to every vulnerability it can reach.', icon: 'cat', author: OWNER }
]

// The write-ups behind the programs, one example until the first is written.
export type Paper = { title: string; detail: string; href: string }

export const papers: Paper[] = [
  { title: 'Rails cracker', detail: 'How a code graph leads to every vulnerability in a repo', href: '/gallery/rails-cracker' }
]
