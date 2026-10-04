import { OWNER } from '../lib/account/handle'

// Newest first, each one a page of its own under /programs.
export type Program = { slug: string; name: string; description: string; author: string }

export const programs: Program[] = [
  { slug: 'rails-cracker', name: 'Rails cracker', description: 'Paste any git repo and follow its code graph to every vulnerability it can reach.', author: OWNER }
]
