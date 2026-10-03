// Minimal typings for the Node built-ins the benchmarks use, so
// `tsc -p benchmark/tsconfig.json` can check them. The package does not depend
// on @types/node (see ts/node_builtins.d.ts); drop this file if it ever does.

declare namespace NodeJS {
  interface ErrnoException extends Error {
    code?: string
  }
}

declare const process: {
  argv: string[]
  env: Record<string, string | undefined>
  exit(code?: number): never
  exitCode: number | undefined
  hrtime: { bigint(): bigint }
}

declare module 'node:child_process' {
  export interface SpawnSyncReturns {
    error?: Error
    status: number | null
  }
  export function spawnSync(
    command: string,
    args: ReadonlyArray<string>,
    options?: { cwd?: string; stdio?: 'inherit' | 'pipe' | 'ignore' },
  ): SpawnSyncReturns
}

declare module 'node:crypto' {
  interface Hash {
    update(data: string | Uint8Array): Hash
    digest(encoding: 'hex'): string
  }
  export function createHash(algorithm: string): Hash
}

declare module 'node:fs' {
  const fs: {
    existsSync(path: string): boolean
    mkdirSync(path: string, options?: { recursive?: boolean }): void
    mkdtempSync(prefix: string): string
    readFileSync(path: string): Uint8Array
    readFileSync(path: string, encoding: 'utf8'): string
    rmSync(path: string, options?: { recursive?: boolean; force?: boolean }): void
    writeFileSync(path: string, data: string): void
  }
  export default fs
}

declare module 'node:os' {
  const os: { tmpdir(): string }
  export default os
}

declare module 'node:path' {
  const path: {
    dirname(path: string): string
    join(...paths: string[]): string
    resolve(...paths: string[]): string
  }
  export default path
}
