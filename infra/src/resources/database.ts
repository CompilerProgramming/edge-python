import { execFileSync } from 'node:child_process'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { client, account_id } from '../client'
import { DB_NAME, ENV, SITE_DIR, keeps } from '../constants'

export const SCHEMA = `${SITE_DIR}db/schema.sql`
const SEED = `${SITE_DIR}db/seed.sql`
const MIGRATIONS = `${SITE_DIR}db/migrations/`

// The table wrangler keeps applied migrations in, created the same way so it adopts it.
const LEDGER = 'create table if not exists d1_migrations (id integer primary key autoincrement, name text unique, applied_at timestamp default current_timestamp not null)'

// What still waits for production, in the order wrangler applies it.
export const migrations = () => (existsSync(MIGRATIONS) ? readdirSync(MIGRATIONS).filter((file) => file.endsWith('.sql')).sort() : [])
export const migration = (file: string) => readFileSync(`${MIGRATIONS}${file}`, 'utf8')

export async function query<T>(id: string, sql: string) {
  for await (const result of client.d1.database.query(id, { account_id, sql })) return (result.results ?? []) as T[]
  return [] as T[]
}

export async function find_database() {
  for await (const db of client.d1.database.list({ account_id, name: DB_NAME })) {
    if (db.name === DB_NAME) return db.uuid!
  }
}

// A new database is born from the schema, so every migration already in it counts as applied.
async function build(id: string) {
  await query(id, readFileSync(SCHEMA, 'utf8'))
  await query(id, readFileSync(SEED, 'utf8'))
  await query(id, LEDGER)
  for (const file of migrations()) await query(id, `insert into d1_migrations (name) values ('${file}')`)
}

// Never deleted, only emptied. Dropping a D1 is irreversible and would change the binding id.
export async function ensure_database() {
  const found = await find_database()
  if (found) return found

  console.log(`Creating D1 "${DB_NAME}"...`)
  const db = await client.d1.database.create({ account_id, name: DB_NAME })
  await build(db.uuid!)
  return db.uuid!
}

// The one way a promote touches the database, so production is only ever migrated, never emptied.
export const update_database = () => (keeps(ENV) ? migrate_database() : reset_database())

// Dev holds nothing worth keeping, so it is rebuilt from the schema, and production refuses before any query.
export async function reset_database() {
  if (keeps(ENV)) throw new Error(`D1 "${DB_NAME}" is production, which is only ever migrated, never emptied.`)
  const id = await ensure_database()

  const tables = await query<{ name: string }>(id, "select name from sqlite_master where type = 'table' and name not like '\\_cf\\_%' escape '\\' and name not like 'sqlite\\_%' escape '\\'")
  // One batch with the foreign keys checked at its end, so a table others still reference can drop first.
  if (tables.length) await query(id, ['pragma defer_foreign_keys = true', ...tables.map(({ name }) => `drop table if exists "${name}"`)].join(';\n'))

  await build(id)
  console.log(`D1 "${DB_NAME}" rebuilt from the schema and seeded.`)
}

// Production keeps its rows, so only the migrations it has not recorded yet run.
function migrate_database() {
  if (migrations().length === 0) return console.log(`D1 "${DB_NAME}" has no migrations waiting.`)

  execFileSync('npm', ['run', 'config'], { cwd: SITE_DIR, stdio: 'inherit' })
  execFileSync('npx', ['wrangler', 'd1', 'migrations', 'apply', DB_NAME, '--remote'], { cwd: SITE_DIR, stdio: 'inherit' })
}
