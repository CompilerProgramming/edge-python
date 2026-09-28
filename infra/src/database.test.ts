import { strict as assert } from 'node:assert'
import { test } from 'node:test'

// Production without credentials, so a lost guard fails here instead of reaching a real database.
delete process.env.CLOUDFLARE_API_TOKEN
process.env.EDGE_ENV = 'prod'
const { keeps } = await import('./constants')
const { reset_database } = await import('./resources/database')

test('dev is emptied on every promote and production never is', async () => {
  assert.equal(keeps('dev'), false)
  assert.equal(keeps('prod'), true)
  await assert.rejects(reset_database(), /never emptied/)
})
