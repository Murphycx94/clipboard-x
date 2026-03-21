import { readFileSync, writeFileSync } from 'fs'
import { resolve, dirname } from 'path'
import { fileURLToPath } from 'url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const root = resolve(__dirname, '..')

let version = process.argv[2]
if (!version) {
  const pkgForVersion = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf-8'))
  const parts = pkgForVersion.version.split('.')
  parts[2] = String(Number(parts[2]) + 1)
  version = parts.join('.')
  console.log(`Auto-incrementing patch version: ${pkgForVersion.version} -> ${version}`)
}

// package.json
const pkgPath = resolve(root, 'package.json')
const pkg = JSON.parse(readFileSync(pkgPath, 'utf-8'))
pkg.version = version
writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + '\n')
console.log(`✓ package.json -> ${version}`)

// tauri.conf.json
const tauriConfPath = resolve(root, 'src-tauri/tauri.conf.json')
const tauriConf = JSON.parse(readFileSync(tauriConfPath, 'utf-8'))
tauriConf.version = version
writeFileSync(tauriConfPath, JSON.stringify(tauriConf, null, 2) + '\n')
console.log(`✓ src-tauri/tauri.conf.json -> ${version}`)

// Cargo.toml
const cargoPath = resolve(root, 'src-tauri/Cargo.toml')
let cargo = readFileSync(cargoPath, 'utf-8')
cargo = cargo.replace(/^version = ".*"/m, `version = "${version}"`)
writeFileSync(cargoPath, cargo)
console.log(`✓ src-tauri/Cargo.toml -> ${version}`)
