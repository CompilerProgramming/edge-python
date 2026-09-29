declare namespace App {
  interface Locals {
    user: import('./lib/account/auth').Me | null
  }
}

declare namespace Cloudflare {
  interface Env {
    OAUTH_GITHUB_ID: string
    OAUTH_GITHUB_SECRET: string
    OAUTH_GOOGLE_ID: string
    OAUTH_GOOGLE_SECRET: string
  }
}

// The compiler, bundled into the Worker as a compiled module for the rules it holds.
declare module '*.wasm?module' {
  const compiler: WebAssembly.Module
  export default compiler
}
