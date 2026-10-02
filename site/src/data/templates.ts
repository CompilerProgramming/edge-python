import { OWNER } from '../lib/account/handle'

// Newest first, each one a page of its own under /templates.
export type Template = { slug: string; name: string; description: string; author: string }

export const templates: Template[] = [
  { slug: 'rails-cracker', name: 'Rails cracker', description: 'Paste a GitHub repo and follow its code graph to every vulnerability it can reach.', author: OWNER }
]
