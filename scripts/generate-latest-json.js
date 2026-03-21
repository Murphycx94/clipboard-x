import { readFileSync, writeFileSync, readdirSync } from 'fs'
import { resolve, dirname } from 'path'
import { fileURLToPath } from 'url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const root = resolve(__dirname, '..')

const version = process.argv[2]
if (!version) {
  console.error('Usage: node scripts/generate-latest-json.js <version>')
  process.exit(1)
}

const repoUrl = 'https://github.com/Murphycx94/clipboard-x'
const baseDownloadUrl = `${repoUrl}/releases/download/v${version}`

function readSig(path) {
  try {
    return readFileSync(path, 'utf-8').trim()
  } catch {
    return null
  }
}

function findFile(dir, ext) {
  try {
    const files = readdirSync(dir)
    const found = files.find((f) => f.endsWith(ext))
    return found ? resolve(dir, found) : null
  } catch {
    return null
  }
}

const bundleDir = resolve(root, 'src-tauri/target/release/bundle')
const platforms = {}

// macOS arm64
const macosDir = resolve(bundleDir, 'macos')
const macArmTar = findFile(macosDir, '.app.tar.gz')
const macArmSig = macArmTar ? readSig(macArmTar + '.sig') : null
if (macArmSig && macArmTar) {
  const filename = macArmTar.split('/').pop()
  platforms['darwin-aarch64'] = {
    signature: macArmSig,
    url: `${baseDownloadUrl}/${filename}`,
  }
  platforms['darwin-x86_64'] = {
    signature: macArmSig,
    url: `${baseDownloadUrl}/${filename}`,
  }
}

// Windows x86_64 (NSIS)
const nsisDir = resolve(bundleDir, 'nsis')
const winNsisTar = findFile(nsisDir, '-setup.exe')
const winNsisSig = winNsisTar ? readSig(winNsisTar + '.sig') : null
if (winNsisSig && winNsisTar) {
  const filename = winNsisTar.split('/').pop()
  platforms['windows-x86_64'] = {
    signature: winNsisSig,
    url: `${baseDownloadUrl}/${filename}`,
  }
}

if (Object.keys(platforms).length === 0) {
  console.error('No build artifacts found. Run tauri build first.')
  process.exit(1)
}

const latestJson = {
  version: `v${version}`,
  notes: '',
  pub_date: new Date().toISOString(),
  platforms,
}

const outPath = resolve(root, 'latest.json')
writeFileSync(outPath, JSON.stringify(latestJson, null, 2) + '\n')
console.log(`✓ latest.json generated for v${version}`)
console.log(`  platforms: ${Object.keys(platforms).join(', ')}`)
