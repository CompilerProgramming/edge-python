import type { APIRoute } from 'astro'
import { env } from 'cloudflare:workers'
import { sha256hex } from '../../lib/crypto'
import { json } from '../../lib/server/http'
import { tokenUser } from '../../lib/server/tokens'
import { roomFor, userById } from '../../lib/server/users'
import { OWNER } from '../../lib/account/handle'
import { bytes as sized } from '../../lib/format'
import type { Packed } from '../../lib/server/bundle'
import { packed } from '../../lib/server/bundle'
import type { Page } from '../../lib/server/packages'
import { checkManifests } from '../../lib/server/engine'
import { MAX_ARTIFACT, MAX_NEW_NAMES, MAX_NEW_VERSIONS, MAX_NOTICE, OWNER_SCALE, checkPages, claimedToday, keyOf, noticed, packageByName, publish, publishedToday, storedBytes, versionExists } from '../../lib/server/packages'

/* The artifact is the only thing sent. Everything a listing shows is read out of it here, so a publisher declares nothing twice and cannot declare it differently from what they shipped. */
export const POST: APIRoute = async ({ request }) => {
  const userId = await tokenUser(env.DB, request.headers.get('authorization'))
  if (!userId) return json({ error: 'That token is not valid.' }, 401)

  // The body is the artifact itself, which also keeps it out of the origin check a form would meet.
  const bytes = await request.arrayBuffer()

  if (bytes.byteLength === 0) return json({ error: 'Send an artifact.' }, 400)
  if (bytes.byteLength > MAX_ARTIFACT) return json({ error: `An artifact is ${MAX_ARTIFACT} bytes at most.` }, 413)

  // A token holder can hand-build a bundle, so the archive and its pages are held to the same rules the CLI packs under.
  let declared: Packed
  let pages: Page[]

  try {
    declared = packed(new Uint8Array(bytes))
    pages = checkPages(declared.docs)
    checkManifests(declared.manifests, declared.locks)
  } catch (error) {
    return json({ error: (error as Error).message }, 400)
  }

  const { name, version, description, notice } = declared

  // The rules checked each field's shape, a release still has to name itself.
  if (typeof name !== 'string') return json({ error: 'A package names itself in edge.json.' }, 400)
  if (typeof version !== 'string') return json({ error: 'A package declares its version in edge.json.' }, 400)
  if (!noticed(notice)) return json({ error: `A license notice is ${MAX_NOTICE} bytes at most.` }, 400)

  const held = await packageByName(env.DB, name)
  if (held && held.user_id !== userId) return json({ error: `The name ${name} belongs to someone else.` }, 409)

  // The owner publishes the standard library in bursts, so no minute's limiter holds it and its day runs larger.
  const owner = (await userById(env.DB, userId))?.handle === OWNER
  const names = owner ? MAX_NEW_NAMES * OWNER_SCALE : MAX_NEW_NAMES
  const versions = owner ? MAX_NEW_VERSIONS * OWNER_SCALE : MAX_NEW_VERSIONS

  // A name nobody holds is the scarce thing, so it costs more than another version of your own.
  const limit = held ? env.PUBLISH_VERSION : env.PUBLISH_NAME
  if (!owner && !(await limit.limit({ key: userId })).success) return json({ error: 'Too many packages published. Try again later.' }, 429)

  if (!held && (await claimedToday(env.DB, userId)) >= names) {
    return json({ error: `You can claim ${names} names a day.` }, 429)
  }

  if ((await publishedToday(env.DB, userId)) >= versions) {
    return json({ error: `You can publish ${versions} versions a day.` }, 429)
  }

  // Refused before the bytes are stored, since nothing reclaims the room afterwards.
  const room = await roomFor(env.DB, userId)
  const used = await storedBytes(env.DB, userId)
  if (used + bytes.byteLength > room) {
    return json({ error: `Your packages use ${sized(used)} of ${sized(room)}. Ask for more room at ${env.SITE}/settings.` }, 413)
  }

  if (await versionExists(env.DB, name, version)) return json({ error: `${name} ${version} is already published.` }, 409)

  const digest = await sha256hex(new Uint8Array(bytes))
  const key = keyOf(name, version)

  // Stored exactly as it arrived, so the digest a manifest pins is the digest of the bytes that ran.
  await env.CDN_BUCKET.put(key, bytes, { httpMetadata: { contentType: 'application/octet-stream' } })
  await publish(env.DB, userId, {
    name,
    version,
    digest,
    size: bytes.byteLength,
    description: typeof description === 'string' ? description : null,
    notice,
    pages
  })

  return json({ name, version, digest, url: `${env.CDN}/${key}` }, 201)
}
