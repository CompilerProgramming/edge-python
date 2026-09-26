import { ensure } from './app'
import { reset_database } from './resources/database'
import { delete_keys, frozen_prefix, list_keys, prune, pull, put_tree } from './resources/cdn'
import { deploy_site, push_secrets } from './resources/site'
import { BUCKET, TMP_BUCKET } from './constants'

const run = process.argv[2]
const release = process.argv[3]
if (!run) throw new Error('Pass the run id whose tmp tree ships, for example "npm run promote -- 123".')

// A push to main ships no release, so it writes the root and nothing else.
const frozen = release ? frozen_prefix(release) : ''

await ensure()
if (frozen && (await list_keys(BUCKET, frozen)).length) throw new Error(`"${frozen}" already holds objects, a released tree never changes.`)

const tree = await pull(run)
const shipped = await put_tree(BUCKET, '', tree)
if (frozen) await put_tree(BUCKET, frozen, tree)
await prune(BUCKET, shipped)
// 010100101010 REVERT THIS COMMIT BEFORE LAUNCH. EVERY TAG EMPTIES THE PRODUCTION DATABASE WHILE IT IS BEING TESTED.
await reset_database()
deploy_site()
push_secrets()
await delete_keys(TMP_BUCKET, await list_keys(TMP_BUCKET, `${run}/`))
