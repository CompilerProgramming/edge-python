import { strict as assert } from 'node:assert'
import { test } from 'node:test'
import { CACHE, FROZEN_CACHE, cache_control, frozen_prefix, swept } from './resources/cdn'

test('a release freezes a copy of its own from 1.0 and none before it', () => {
  assert.equal(frozen_prefix('v0.7.0'), '')
  assert.equal(frozen_prefix('v0.45.12'), '')
  assert.equal(frozen_prefix('v1.0.0'), 'v1.0.0/')
  assert.equal(frozen_prefix('v2.13.4'), 'v2.13.4/')
})

test('a release shaped any other way is refused', () => {
  assert.throws(() => frozen_prefix('v1.0'), /shaped vX\.Y\.Z/)
  assert.throws(() => frozen_prefix('latest'), /shaped vX\.Y\.Z/)
})

test('the root revalidates and a frozen copy is kept for a year', () => {
  assert.equal(cache_control(''), CACHE)
  assert.equal(cache_control('123/'), CACHE)
  assert.equal(cache_control('v1.0.0/'), FROZEN_CACHE)
  assert.match(FROZEN_CACHE, /max-age=31536000, immutable/)
})

test('a promote sweeps a root key the run dropped and spares every frozen one', () => {
  const keys = ['cli/edge-old.tar.gz', 'compiler.wasm', 'v1.0.0/cli/install.sh', 'v1.0.0/compiler.wasm']
  assert.deepEqual(swept(keys, ['compiler.wasm']), ['cli/edge-old.tar.gz'])
})

test('a root key that only opens with a v is swept all the same', () => {
  assert.deepEqual(swept(['v1/old.js', 'vm/compiler.wasm'], []), ['v1/old.js', 'vm/compiler.wasm'])
})
