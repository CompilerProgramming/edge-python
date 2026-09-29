import { execFileSync } from 'node:child_process'
import { copyFileSync, mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { REPO_DIR, SITE_DIR } from '../constants'

// Where the Worker imports the compiler it checks packages with, the path a cargo wasm build writes.
const COMPILER = join(REPO_DIR, 'target/wasm32-unknown-unknown/release/compiler.wasm')

const SECRETS = ['OAUTH_GITHUB_ID', 'OAUTH_GITHUB_SECRET', 'OAUTH_GOOGLE_ID', 'OAUTH_GOOGLE_SECRET']

// Builds and publishes the Worker on the compiler this run tested, wrangler.json routes it.
export function deploy_site(tree: string) {
  mkdirSync(dirname(COMPILER), { recursive: true })
  copyFileSync(join(tree, 'compiler.wasm'), COMPILER)
  execFileSync('npm', ['run', 'deploy'], { cwd: SITE_DIR, stdio: 'inherit' })
}

export function push_secrets() {
  const secrets = Object.fromEntries(SECRETS.map((name) => [name, process.env[name] ?? '']))
  execFileSync('npx', ['wrangler', 'secret', 'bulk'], { cwd: SITE_DIR, input: JSON.stringify(secrets), stdio: ['pipe', 'inherit', 'inherit'] })
}
