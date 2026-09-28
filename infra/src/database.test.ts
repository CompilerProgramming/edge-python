import { strict as assert } from 'node:assert'
import { test } from 'node:test'

// Production without credentials, so a lost guard fails here instead of reaching a real database.
delete process.env.CLOUDFLARE_API_TOKEN
process.env.EDGE_ENV = 'prod'
const { reset_database } = await import('./resources/database')

test('production refuses to be emptied before it asks the database anything', async () => {
  await assert.rejects(reset_database(), /never emptied/)
})
