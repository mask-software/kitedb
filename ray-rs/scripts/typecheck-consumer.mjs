// Type-check the published typings the way a user's project sees them.
//
// For each module setup users pick, this builds a throwaway project that
// depends on @kitedb/core (a symlink to this package, so `exports` and
// `types` resolve as they do from npm), copies in __test__/consumer/consumer.ts
// and runs tsc with `strict` and `skipLibCheck: false`. That checks the
// shipped dist/*.d.ts and the native index.d.ts, which the package's own
// build never does.
//
// Needs dist/ (`bun run build:ts`); does not need the native binary.
import { spawnSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { dirname, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

const packageRoot = join(dirname(fileURLToPath(import.meta.url)), '..')
const require = createRequire(join(packageRoot, 'package.json'))
const tsc = require.resolve('typescript/bin/tsc')
const typesNode = dirname(require.resolve('@types/node/package.json'))
const fixture = join(packageRoot, '__test__', 'consumer', 'consumer.ts')

if (!existsSync(join(packageRoot, 'dist', 'index.d.ts'))) {
  console.error('typecheck-consumer: dist/index.d.ts is missing; run `bun run build:ts` first')
  process.exit(1)
}

const strictOptions = {
  strict: true,
  skipLibCheck: false,
  exactOptionalPropertyTypes: true,
  noUncheckedIndexedAccess: true,
  noEmit: true,
  target: 'ES2022',
  lib: ['ES2022'],
  types: ['node'],
}

const setups = [
  { name: 'ESM, NodeNext', type: 'module', module: 'NodeNext', moduleResolution: 'NodeNext' },
  { name: 'CommonJS, NodeNext', type: 'commonjs', module: 'NodeNext', moduleResolution: 'NodeNext' },
  { name: 'ESM, Bundler', type: 'module', module: 'ESNext', moduleResolution: 'Bundler' },
]

function link(target, path) {
  mkdirSync(dirname(path), { recursive: true })
  // 'junction' only matters on Windows, where a plain directory symlink needs privileges.
  symlinkSync(target, path, 'junction')
}

let failed = 0
for (const setup of setups) {
  const dir = mkdtempSync(join(tmpdir(), 'kitedb-consumer-'))
  try {
    writeFileSync(
      join(dir, 'package.json'),
      JSON.stringify({ name: 'kitedb-consumer', private: true, type: setup.type }, null, 2),
    )
    writeFileSync(
      join(dir, 'tsconfig.json'),
      JSON.stringify(
        {
          compilerOptions: { ...strictOptions, module: setup.module, moduleResolution: setup.moduleResolution },
          files: ['consumer.ts'],
        },
        null,
        2,
      ),
    )
    copyFileSync(fixture, join(dir, 'consumer.ts'))
    link(packageRoot, join(dir, 'node_modules', '@kitedb', 'core'))
    link(typesNode, join(dir, 'node_modules', '@types', 'node'))

    const result = spawnSync(process.execPath, [tsc, '-p', dir, '--pretty', 'false'], {
      cwd: packageRoot,
      encoding: 'utf8',
    })
    if (result.status === 0) {
      console.log(`ok    ${setup.name}`)
    } else {
      failed += 1
      console.log(`FAIL  ${setup.name}`)
      // tsc prints paths relative to this package: dist/index.d.ts, index.d.ts, <consumer>/consumer.ts.
      process.stdout.write(result.stdout.split(relative(packageRoot, dir)).join('<consumer>'))
      process.stderr.write(result.stderr)
    }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

if (failed > 0) {
  console.error(`typecheck-consumer: ${failed} of ${setups.length} setups failed`)
  process.exit(1)
}
