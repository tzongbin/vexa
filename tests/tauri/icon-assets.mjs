import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { readFile, access } from 'node:fs/promises'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const iconDir = resolve(root, 'src-tauri/icons')
const ico = await readFile(resolve(iconDir, 'icon.ico'))
assert.equal(ico.readUInt16LE(0), 0, 'ICO reserved field')
assert.equal(ico.readUInt16LE(2), 1, 'ICO image type')
const entries = ico.readUInt16LE(4)
const sizes = new Set()
for (let index = 0; index < entries; index += 1) {
  const offset = 6 + index * 16
  sizes.add(ico[offset] || 256)
}
for (const size of [16, 32]) assert(sizes.has(size), `icon.ico must include ${size}x${size}`)
for (const path of ['32x32.png', '64x64.png', '128x128.png', '128x128@2x.png', 'icon.png', 'icon.icns', 'StoreLogo.png', 'Square44x44Logo.png', 'android/mipmap-mdpi/ic_launcher.png', 'android/mipmap-mdpi/ic_launcher_foreground.png', 'ios/AppIcon-20x20@1x.png']) {
  await access(resolve(iconDir, path))
}
const svg = await readFile(resolve(root, 'public/vexa-logo.svg'), 'utf8')
assert.match(svg, /#2563eb/i)
assert.match(svg, /<path/)
console.log(`PASS 原始 SVG 與 Tauri 圖示資產：ICO 含 ${[...sizes].sort((a, b) => a - b).join(', ')}px 尺寸，跨平台 PNG/ICNS 均存在`)

const executable = resolve(root, 'src-tauri/target/release/vexa.exe')
try {
  await access(executable)
  execFileSync('powershell.exe', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', resolve(root, 'tests/tauri/verify-exe-icon.ps1'), executable], { stdio: 'inherit' })
} catch (error) {
  if (error?.code === 'ENOENT' && error?.path === executable) {
    console.log('SKIP EXE 內嵌圖示：Release EXE 尚未建置')
  } else {
    throw error
  }
}
