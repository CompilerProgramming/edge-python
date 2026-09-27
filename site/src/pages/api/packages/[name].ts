import type { APIRoute } from 'astro'
import { env } from 'cloudflare:workers'
import { json, tooMany } from '../../../lib/server/http'
import { downloaded, keyOf, named, packageByName, versionsOf } from '../../../lib/server/packages'

// What `edge add <name>` reads, so a manifest entry can carry the digest of the version it pinned.
export const GET: APIRoute = async ({ params, url, request }) => {
  if (await tooMany(env.READ_IP, request)) return json({ error: 'Too many requests. Try again later.' }, 429)

  const name = String(params.name ?? '').toLowerCase()
  if (!named(name)) return json({ error: 'No such package.' }, 404)

  const held = await packageByName(env.DB, name)
  if (!held) return json({ error: 'No such package.' }, 404)

  const { results } = await versionsOf(env.DB, name)

  // A ?v= asks for that version, otherwise the newest one still live.
  const asked = url.searchParams.get('v')
  const release = asked ? results.find((each) => each.version === asked) : results.find((each) => each.yanked_at === null)

  if (asked && !release) return json({ error: `${name} has no version ${asked}.` }, 404)
  if (!release || release.yanked_at !== null) return json({ error: asked ? `${name} ${asked} is yanked.` : 'Every version of that package is yanked.' }, 410)

  // A lock refresh asks the same question again about a package somebody already took, so it counts once.
  if (url.searchParams.get('lock') !== '1') await downloaded(env.DB, name)

  return json({
    name,
    version: release.version,
    digest: release.digest,
    size: release.size,
    // Null until something establishes it, so a consumer reads no claim rather than every host.
    hosts: release.hosts,
    url: `${env.CDN}/${keyOf(name, release.version)}`
  })
}
