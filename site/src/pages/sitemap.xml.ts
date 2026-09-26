import type { APIRoute } from 'astro'
import { getCollection } from 'astro:content'
import { env } from 'cloudflare:workers'
import { tree } from '../lib/docs/tree'
import { indexable } from '../lib/server/packages'

// A package enters once it says what it is and documents it, and its author enters with it.
export const GET: APIRoute = async ({ site, url }) => {
  const origin = (site ?? url).origin
  const docs = tree(await getCollection('docs'), '').flatMap((section) => section.docs)
  const { results: packages } = await indexable(env.DB)
  const people = new Set(packages.map((each) => `/@${each.handle}`))
  const paths = ['/', ...docs.map((doc) => `/docs/${doc.slug}`), ...packages.map((each) => `/package/${each.name}`), ...people]

  const body = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${paths.map((path) => `  <url><loc>${origin}${path}</loc></url>`).join('\n')}
</urlset>
`

  return new Response(body, { headers: { 'content-type': 'application/xml' } })
}
