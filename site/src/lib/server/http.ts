import type { APIContext } from 'astro'

export const json = (data: unknown, status = 200) => Response.json(data, { status })

export async function body<T extends Record<string, unknown>>(request: Request): Promise<Partial<T>> {
  try {
    return (await request.json()) as Partial<T>
  } catch {
    return {}
  }
}

export const ip = (context: APIContext) => context.request.headers.get('cf-connecting-ip') ?? context.clientAddress

/* Whether this caller has asked too often, keyed by the address since nothing here needs an account. */
export async function tooMany(limiter: RateLimit, request: Request) {
  const ip = request.headers.get('cf-connecting-ip')
  return ip ? !(await limiter.limit({ key: ip })).success : false
}
