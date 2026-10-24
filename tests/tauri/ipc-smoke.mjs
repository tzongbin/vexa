import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { createServer } from 'node:http'
import { once } from 'node:events'
import { cp, copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, resolve } from 'node:path'
import { promisify } from 'node:util'
import { chromium } from '@playwright/test'
import { createServer as createTcpServer } from 'node:net'

const sourceExecutable = resolve('src-tauri/target/release/vexa.exe')
let executable = sourceExecutable
const execFileAsync = promisify(execFile)
const requestCounts = new Map()
const appServer = createServer((request, response) => {
  const url = new URL(request.url ?? '/', 'http://localhost')
  requestCounts.set(url.pathname, (requestCounts.get(url.pathname) ?? 0) + 1)
  if (url.pathname === '/delay') {
    const delayMs = Number(url.searchParams.get('ms') ?? 1500)
    const timer = setTimeout(() => {
      response.writeHead(200, { 'content-type': 'text/plain; charset=utf-8' })
      response.end('finished after delay')
    }, delayMs)
    request.on('aborted', () => clearTimeout(timer))
    response.on('close', () => clearTimeout(timer))
    return
  }
  if (url.pathname === '/html') {
    response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' })
    response.end(`<script>window.parent.__VEXA_PWNED__=true;window.parent.__TAURI_INTERNALS__.invoke('send_request',{})</script><img src="http://127.0.0.1:${appPort}/leak" onerror="window.parent.__VEXA_PWNED__=true">`)
    return
  }
  if (url.pathname === '/long-text' || url.pathname === '/other-long-text') {
    response.writeHead(200, { 'content-type': 'text/plain; charset=utf-8' })
    response.end(Array.from({ length: 160 }, (_, index) => `line ${index + 1}: ${'WebView2 Source wrapping '.repeat(8)}`).join('\n'))
    return
  }
  if (url.pathname === '/leak') {
    response.writeHead(200, { 'content-type': 'image/png' })
    response.end('blocked resource fixture')
    return
  }

  const chunks = []
  request.on('data', (chunk) => chunks.push(chunk))
  request.on('end', () => {
    const status = url.pathname === '/status/500' ? 500 : 200
    response.writeHead(status, {
      'content-type': 'application/json; charset=utf-8',
      'set-cookie': ['fixture=one; HttpOnly', 'fixture2=two; Secure'],
    })
    response.end(JSON.stringify({
      method: request.method,
      path: url.pathname,
      query: [...url.searchParams.entries()],
      body: Buffer.concat(chunks).toString('utf8'),
    }))
  })
})

async function getFreePort() {
  const server = createTcpServer()
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const address = server.address()
  assert(address && typeof address === 'object')
  await new Promise((resolveClose, rejectClose) => server.close((error) => error ? rejectClose(error) : resolveClose()))
  return address.port
}

async function waitForDevTools(port, child) {
  const endpoint = `http://127.0.0.1:${port}/json/version`
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`Vexa exited during startup with code ${child.exitCode}`)
    try {
      const response = await fetch(endpoint)
      if (response.ok) return
    } catch { /* WebView2 has not opened its DevTools endpoint yet. */ }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 200))
  }
  throw new Error('WebView2 DevTools endpoint did not appear; true Tauri IPC could not be exercised.')
}

function launchApp() {
  child = spawn(executable, [], {
    cwd: dirname(executable),
    windowsHide: true,
    env: {
      ...process.env,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debuggerPort}`,
      WEBVIEW2_USER_DATA_FOLDER: profileDirectory,
    },
  })
  child.stderr?.on('data', (chunk) => { stderr = `${stderr}${chunk}`.slice(-4000) })
}

async function connectAppPage() {
  await waitForDevTools(debuggerPort, child)
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${debuggerPort}`)
  const deadline = Date.now() + 10_000
  let page
  while (Date.now() < deadline && !page) {
    page = browser.contexts().flatMap((context) => context.pages()).find((candidate) => candidate.url().startsWith('http'))
    if (!page) await new Promise((resolveDelay) => setTimeout(resolveDelay, 100))
  }
  assert(page, 'Tauri WebView page should be exposed through WebView2 DevTools')
  await page.waitForSelector('.app-shell', { timeout: 10_000 })
  return page
}

async function ctrlWheel(page, selector, deltaY) {
  const box = await page.locator(selector).boundingBox()
  assert(box, `${selector} must be visible for Ctrl+Wheel verification`)
  const x = box.x + box.width / 2
  const y = box.y + Math.min(20, box.height / 2)
  const session = await page.context().newCDPSession(page)
  try {
    await page.mouse.move(x, y)
    await session.send('Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX: 0, deltaY, modifiers: 2 })
  } finally {
    await session.detach()
  }
}

async function stopApp() {
  const processToStop = child
  child = undefined
  if (!processToStop || processToStop.exitCode !== null) return
  const exited = once(processToStop, 'exit').catch(() => undefined)
  try {
    const closeWindow = `Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class VexaSmokeWindow { [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam); }'; $p = Get-Process -Id ${processToStop.pid}; if ($p.MainWindowHandle -ne [IntPtr]::Zero) { [VexaSmokeWindow]::PostMessage($p.MainWindowHandle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null }`
    await execFileAsync('powershell', ['-NoProfile', '-Command', closeWindow], { windowsHide: true })
  } catch {
    // Fall back to terminating the process below if a window message is unavailable.
  }
  await Promise.race([exited, new Promise((resolveDelay) => setTimeout(resolveDelay, 5000))])
  if (processToStop.exitCode === null) {
    try {
      await execFileAsync('taskkill', ['/PID', String(processToStop.pid), '/T', '/F'], { windowsHide: true })
    } catch {
      processToStop.kill()
    }
    await Promise.race([exited, new Promise((resolveDelay) => setTimeout(resolveDelay, 5000))])
  }
}

let child
let browser
let profileDirectory
const temporaryDirectories = []
let aclDirectory
let aclIdentity
let appPort
let debuggerPort
let stderr = ''

async function createTemporaryDirectory(prefix) {
  const path = await mkdtemp(resolve(tmpdir(), prefix))
  temporaryDirectories.push(path)
  return path
}

async function waitForSettings(path, predicate) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const settings = JSON.parse(await readFile(path, 'utf8'))
      if (predicate(settings)) return settings
    } catch { /* The first atomic save may not have completed yet. */ }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 50))
  }
  throw new Error(`Settings file did not reach the expected state: ${path}`)
}

async function waitForJsonFile(path, predicate) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const value = JSON.parse(await readFile(path, 'utf8'))
      if (predicate(value)) return value
    } catch { /* Atomic write or first file creation may still be pending. */ }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 50))
  }
  throw new Error(`JSON file did not reach the expected state: ${path}`)
}

try {
  appPort = await new Promise((resolveListen, rejectListen) => {
    appServer.once('error', rejectListen)
    appServer.listen(0, '127.0.0.1', () => resolveListen(appServer.address().port))
  })
  debuggerPort = await getFreePort()
  const portableDirectory = await createTemporaryDirectory('vexa-portable-')
  executable = resolve(portableDirectory, 'vexa.exe')
  await copyFile(sourceExecutable, executable)
  profileDirectory = await createTemporaryDirectory('vexa-tauri-webview2-')
  launchApp()
  let page = await connectAppPage()

  const settingsPath = resolve(portableDirectory, 'Data/settings.json')
  let settingsExists = true
  try { await readFile(settingsPath) } catch (error) { if (error?.code === 'ENOENT') settingsExists = false; else throw error }
  assert.equal(settingsExists, false, 'default preferences must not create Data/settings.json')
  await page.evaluate(() => {
    localStorage.setItem('vexa.ui.fontSize', 'small')
    localStorage.setItem('vexa.ui.theme', 'dark')
    localStorage.setItem('vexa.response.sourceFontSize', '14')
  })
  await page.reload()
  await page.waitForFunction(() => document.querySelector('.app-shell')?.getAttribute('data-font-size') === 'small')
  assert.equal(await page.locator('.app-shell').getAttribute('data-theme'), 'dark')
  const migratedSettings = JSON.parse(await readFile(settingsPath, 'utf8'))
  assert.deepEqual(migratedSettings, { version: 1, appearance: { theme: 'dark', fontSize: 'small', sourceFontSize: 14 }, historyLimit: 1000 })
  assert.equal(await page.evaluate(() => localStorage.getItem('vexa.ui.fontSize')), null)
  await page.evaluate(() => {
    localStorage.setItem('vexa.ui.fontSize', 'xlarge')
    localStorage.setItem('vexa.ui.theme', 'light')
    localStorage.setItem('vexa.response.sourceFontSize', '30')
  })
  await page.reload()
  await page.waitForFunction(() => document.querySelector('.app-shell')?.getAttribute('data-font-size') === 'small')
  assert.equal(await page.locator('.app-shell').getAttribute('data-theme'), 'dark')
  assert.deepEqual(JSON.parse(await readFile(settingsPath, 'utf8')), migratedSettings, 'existing JSON must take precedence over remaining legacy browser values')
  assert.equal(await page.evaluate(() => localStorage.getItem('vexa.ui.fontSize')), null)
  console.log('PASS Portable 設定：預設不建檔、舊偏好遷移，既有 JSON 優先於 localStorage')

  await page.getByLabel('Request URL').fill(`127.0.0.1:${appPort}/json?name=%E6%B8%AC%E8%A9%A6`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForFunction(() => document.querySelector('.status-pill')?.textContent?.includes('200 OK'))
  assert.equal(await page.getByLabel('Request URL').inputValue(), `http://127.0.0.1:${appPort}/json?name=%E6%B8%AC%E8%A9%A6`)
  const jsonBody = await page.locator('.response-code').innerText()
  assert.match(jsonBody, /測試/)
  assert.match(jsonBody, /"method": "GET"/)
  console.log('PASS 真正 Tauri IPC：Request 經 Rust HTTP Engine 送至動態 localhost Server，Response 回到原 Tab')

  await page.getByRole('button', { name: 'Auth', exact: true }).click()
  await page.locator('.request-pane').getByLabel('類型').selectOption('bearer')
  await page.getByPlaceholder('Bearer token').fill('vexa-smoke-token-never-persist')
  await page.getByRole('button', { name: 'Body', exact: true }).first().click()
  await page.locator('.body-type').selectOption('json')
  await page.getByLabel('Request Body 編輯器').fill('{"privateKey":"vexa-smoke-body-never-persist"}')
  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/json`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForFunction(() => document.querySelector('.status-pill')?.textContent?.includes('200 OK'))
  const historyPath = resolve(portableDirectory, 'Data/history.json')
  const sessionPath = resolve(portableDirectory, 'Data/session.json')
  const savedHistory = await waitForJsonFile(historyPath, (history) => history.entries?.length > 0)
  const latestRequest = savedHistory.entries[0].request
  assert.equal(latestRequest.auth.token, '')
  assert.equal(latestRequest.body.text, '')
  assert.equal(latestRequest.needsSensitiveReview, true)
  assert.equal(JSON.stringify(savedHistory).includes('vexa-smoke-token-never-persist'), false)
  assert.equal(JSON.stringify(savedHistory).includes('vexa-smoke-body-never-persist'), false)
  assert.equal(savedHistory.entries.some((entry) => 'response' in entry || 'responseBody' in entry), false)
  await page.getByRole('button', { name: 'History' }).click()
  const beforeHistoryOpen = requestCounts.get('/json') ?? 0
  const tabCountBeforeHistory = await page.locator('.tabs .tab').count()
  await page.locator('.history-entry').first().click()
  await page.waitForFunction(() => document.querySelector('.response-empty'))
  assert.equal(requestCounts.get('/json') ?? 0, beforeHistoryOpen, 'opening a History item must not send it')
  assert.equal(await page.locator('.tabs .tab').count(), tabCountBeforeHistory + 1)
  await page.locator('.history-entry').first().click()
  assert.equal(await page.locator('.tabs .tab').count(), tabCountBeforeHistory + 1, 'opening the same History item again selects its existing Tab')
  await page.locator('.tabs .tab').first().click()
  const savedSession = await waitForJsonFile(sessionPath, (session) => session.tabs?.some((tab) => tab.request.url.endsWith('/json') && tab.request.auth.kind === 'bearer' && tab.request.needsSensitiveReview))
  assert.equal(JSON.stringify(savedSession).includes('vexa-smoke-token-never-persist'), false)
  assert.equal(JSON.stringify(savedSession).includes('vexa-smoke-body-never-persist'), false)
  assert.equal(savedSession.tabs.some((tab) => 'response' in tab), false)
  console.log('PASS 真正 Tauri IPC：History 僅保存脫敏 Request、Body 預設排除、還原不送出且不保存 Response')
  await page.getByRole('button', { name: '新增 Tab' }).click()

  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/status/500`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForFunction(() => document.querySelector('.status-pill')?.textContent?.includes('500 Internal Server Error'))
  console.log('PASS 真正 Tauri IPC：HTTP 500 作為 Response 返回')

  await page.getByLabel('Timeout 毫秒').fill('100')
  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/delay?ms=1500`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForSelector('.error-timeout', { timeout: 5_000 })
  await page.waitForFunction(() => !document.querySelector('.tab-spinner'))
  console.log('PASS 真正 Tauri IPC：Timeout 會終止並清除 Loading')

  await page.getByLabel('Timeout 毫秒').fill('5000')
  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/delay?ms=4000`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.getByRole('button', { name: /取消/ }).waitFor({ state: 'visible', timeout: 5_000 })
  await page.getByRole('button', { name: /取消/ }).click()
  await page.waitForSelector('.error-canceled', { timeout: 5_000 })
  await page.waitForFunction(() => !document.querySelector('.tab-spinner'))
  console.log('PASS 真正 Tauri IPC：Cancel 取消 Rust HTTP 任務並清除 Loading')

  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/json`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForFunction(() => document.querySelector('.status-pill')?.textContent?.includes('200 OK'))
  assert.match(await page.locator('.response-code').innerText(), /"method": "GET"/)
  console.log('PASS 真正 Tauri IPC：取消/逾時後仍可重新 Send')

  const slowCountBefore = requestCounts.get('/delay') ?? 0
  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/delay?ms=1200`)
  await page.getByRole('button', { name: /Send/ }).click()
  const slowTab = page.getByRole('button', { name: /GET \/delay/ })
  await slowTab.locator('.tab-spinner').waitFor({ state: 'visible', timeout: 5_000 })
  await page.keyboard.press('Control+Enter')
  await page.getByRole('button', { name: '新增 Tab' }).click()
  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/json`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForFunction(() => document.querySelector('.status-pill')?.textContent?.includes('200 OK'))
  assert.match(await page.locator('.response-code').innerText(), /"method": "GET"/)
  await page.getByRole('button', { name: '新增 Tab' }).click()
  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/status/500`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForFunction(() => document.querySelector('.status-pill')?.textContent?.includes('500 Internal Server Error'))
  await slowTab.locator('.tab-spinner').waitFor({ state: 'hidden', timeout: 5_000 })
  assert.equal(requestCounts.get('/delay'), slowCountBefore + 1, 'Ctrl+Enter must not duplicate a running request')
  await slowTab.click()
  assert.match(await page.locator('.response-code').innerText(), /finished after delay/)
  await page.getByRole('button', { name: /GET \/json/ }).last().click()
  assert.match(await page.locator('.response-code').innerText(), /"method": "GET"/)
  await page.getByRole('button', { name: /GET \/status\/500/ }).last().click()
  assert.match(await page.locator('.status-pill').innerText(), /500 Internal Server Error/)
  console.log('PASS 真正 Tauri IPC：三個 Tab 各自保存 Response，延遲完成不覆蓋其他 Tab，且不重複送出')

  await page.getByRole('button', { name: '新增 Tab' }).click()
  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/html`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForFunction(() => document.querySelector('.status-pill')?.textContent?.includes('200 OK'))
  await page.getByRole('button', { name: 'Preview' }).click()
  const preview = page.getByTitle('隔離的 HTML 預覽')
  assert.equal(await preview.getAttribute('sandbox'), '')
  assert.equal(await page.frameLocator('.html-preview').locator('body').evaluate(() => window.origin), 'null')
  await page.waitForTimeout(200)
  assert.equal(await page.evaluate(() => window.__VEXA_PWNED__ ?? false), false)
  assert.equal(requestCounts.get('/leak') ?? 0, 0, 'HTML preview must not load external resources')
  console.log('PASS 真正 WebView2：不可信 HTML 無 Script/Tauri 權限，且 CSP 阻止外部資源')

  await page.getByRole('button', { name: '字體大小設定' }).click()
  await page.locator('.font-menu-title').click()
  assert.equal(await page.locator('.font-menu').count(), 1, 'clicking inside the settings menu must keep it open')
  await page.keyboard.press('Escape')
  assert.equal(await page.locator('.font-menu').count(), 0, 'Escape must close the settings menu')
  await page.getByRole('button', { name: '字體大小設定' }).click()
  await page.locator('.request-pane h1').click()
  assert.equal(await page.locator('.font-menu').count(), 0, 'outside pointer must close the settings menu')
  for (const [label, size, pixels] of [['Small', 'small', 14], ['Normal', 'normal', 16], ['Large', 'large', 18], ['XLarge', 'xlarge', 20]]) {
    await page.getByRole('button', { name: '字體大小設定' }).click()
    await page.getByRole('button', { name: new RegExp(`^${label}`) }).click()
    assert.equal(await page.locator('.app-shell').getAttribute('data-font-size'), size)
    assert.equal(await page.locator('.app-shell').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize)), pixels)
    assert.equal(await page.locator('.font-menu').count(), 0, 'selecting a font size must close the settings menu')
    for (const timeoutValue of ['30000', '120000', '300000']) {
      await page.getByLabel('Timeout 毫秒').fill(timeoutValue)
      const visible = await page.getByLabel('Timeout 毫秒').evaluate((input) => {
        const context = document.createElement('canvas').getContext('2d')
        context.font = getComputedStyle(input).font
        const inputBounds = input.getBoundingClientRect()
        const unitBounds = document.querySelector('.timeout-unit').getBoundingClientRect()
        const titleBounds = document.querySelector('.request-pane h1').getBoundingClientRect()
        const labelBounds = document.querySelector('.timeout-label').getBoundingClientRect()
        return inputBounds.width >= context.measureText(input.value).width + 4
          && unitBounds.left - inputBounds.right >= 3
          && labelBounds.left > titleBounds.right
      })
      assert(visible, `Timeout ${timeoutValue} must fit at ${pixels}px without overlapping its unit or heading`)
    }
  }

  await page.getByRole('button', { name: '字體大小設定' }).click()
  await page.getByRole('button', { name: /^Small/ }).click()
  await page.getByRole('button', { name: '新增 Tab' }).click()
  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/long-text`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForFunction(() => document.querySelector('.response-code')?.textContent?.includes('line 1:'))
  assert.equal(await page.locator('.response-code').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize)), 14)
  const pageMetrics = await page.evaluate(() => ({ width: innerWidth, pixelRatio: devicePixelRatio, viewportScale: visualViewport?.scale }))
  await ctrlWheel(page, '.url-input', -100)
  assert.equal(await page.locator('.response-code').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize)), 14)
  assert.equal(await page.evaluate(() => localStorage.getItem('vexa.response.sourceFontSize')), null)
  assert.deepEqual(await page.evaluate(() => ({ width: innerWidth, pixelRatio: devicePixelRatio, viewportScale: visualViewport?.scale })), pageMetrics, 'Ctrl+Wheel outside Source must not zoom the WebView2 page')
  await ctrlWheel(page, '.response-code', -100)
  await page.waitForFunction(() => getComputedStyle(document.querySelector('.response-code')).fontSize === '15px')
  assert.deepEqual(await page.evaluate(() => ({ width: innerWidth, pixelRatio: devicePixelRatio, viewportScale: visualViewport?.scale })), pageMetrics, 'Ctrl+Wheel on Source must not zoom the WebView2 page')
  console.log('PASS 真正 WebView2：Source Ctrl+Wheel 增加 1px，頁面縮放維持不變')

  await page.getByRole('button', { name: '新增 Tab' }).click()
  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/other-long-text`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForFunction(() => document.querySelector('.response-code')?.textContent?.includes('line 1:'))
  assert.equal(await page.locator('.response-code').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize)), 15)
  await page.getByRole('button', { name: /GET \/long-text/ }).click()
  assert.equal(await page.locator('.response-code').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize)), 15)
  console.log('PASS 真正 WebView2：切換 Tab 後 Response Source 字級一致')

  await page.getByRole('button', { name: '字體大小設定' }).click()
  await page.getByRole('button', { name: /^XLarge/ }).click()
  assert.equal(await page.locator('.app-shell').getAttribute('data-font-size'), 'xlarge')
  assert.equal(await page.locator('.response-code').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize)), 15)
  await page.locator('.theme-toggle').click()
  const persisted = await waitForSettings(settingsPath, (settings) => settings.appearance.fontSize === 'xlarge' && settings.appearance.sourceFontSize === 15 && settings.appearance.theme === 'system')
  assert.deepEqual(persisted.appearance, { theme: 'system', fontSize: 'xlarge', sourceFontSize: 15 })
  const expectedActiveTabId = await page.locator('.tabs .tab-active').getAttribute('data-tab-id')
  const expectedTabCount = await page.locator('.tabs .tab').count()
  const expectedSession = await waitForJsonFile(sessionPath, (session) => session.tabs?.length === expectedTabCount && session.activeTabId === expectedActiveTabId)
  const expectedActiveUrl = await page.getByLabel('Request URL').inputValue()
  const persistedHistory = JSON.parse(await readFile(historyPath, 'utf8'))
  assert.equal(persistedHistory.entries.every((entry) => !('response' in entry)), true)
  assert.equal(JSON.stringify(persistedHistory).includes('vexa-smoke-token-never-persist'), false)
  assert.equal(JSON.stringify(expectedSession).includes('vexa-smoke-body-never-persist'), false)
  await browser.close().catch(() => undefined)
  browser = undefined
  await stopApp()
  launchApp()
  page = await connectAppPage()
  assert.equal(await page.locator('.app-shell').getAttribute('data-font-size'), 'xlarge')
  assert.equal(await page.locator('.app-shell').getAttribute('data-theme'), 'system')
  await page.waitForFunction(({ count, id }) => document.querySelectorAll('.tabs .tab').length === count && document.querySelector('.tabs .tab-active')?.getAttribute('data-tab-id') === id, { count: expectedTabCount, id: expectedActiveTabId })
  assert.equal(await page.getByLabel('Request URL').inputValue(), expectedActiveUrl)
  await page.locator('.response-empty').waitFor({ state: 'visible' })
  const requestCountBeforeHistoryReload = [...requestCounts.values()].reduce((total, count) => total + count, 0)
  await page.getByRole('button', { name: 'History' }).click()
  await page.locator('.history-entry').first().waitFor({ state: 'visible', timeout: 5_000 })
  assert.equal([...requestCounts.values()].reduce((total, count) => total + count, 0), requestCountBeforeHistoryReload)
  await page.getByRole('button', { name: '收合 History' }).click()
  console.log('PASS 真正 WebView2：重啟後 History 仍存在，載入 History 不會重新發送 Request')
  await page.getByLabel('Request URL').fill(`http://127.0.0.1:${appPort}/long-text`)
  await page.getByRole('button', { name: /Send/ }).click()
  await page.waitForFunction(() => document.querySelector('.response-code')?.textContent?.includes('line 1:'))
  assert.equal(await page.locator('.response-code').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize)), 15)
  console.log('PASS 真正 WebView2：重新啟動 Release EXE 還原全域字級、Source 字級與主題')

  await browser.close().catch(() => undefined)
  browser = undefined
  await stopApp()
  const copiedPortableDirectory = await createTemporaryDirectory('vexa-portable-copy-')
  executable = resolve(copiedPortableDirectory, 'vexa.exe')
  await copyFile(sourceExecutable, executable)
  await cp(resolve(portableDirectory, 'Data'), resolve(copiedPortableDirectory, 'Data'), { recursive: true })
  profileDirectory = await createTemporaryDirectory('vexa-tauri-webview2-copy-')
  debuggerPort = await getFreePort()
  launchApp()
  page = await connectAppPage()
  assert.equal(await page.locator('.app-shell').getAttribute('data-font-size'), 'xlarge')
  assert.equal(await page.locator('.app-shell').getAttribute('data-theme'), 'system')
  console.log('PASS Portable 複製：EXE 與 Data/settings.json 複製後仍還原設定')

  await browser.close().catch(() => undefined)
  browser = undefined
  await stopApp()
  const corruptDirectory = await createTemporaryDirectory('vexa-corrupt-settings-')
  executable = resolve(corruptDirectory, 'vexa.exe')
  await copyFile(sourceExecutable, executable)
  await mkdir(resolve(corruptDirectory, 'Data'))
  const corruptBytes = '{invalid json'
  await writeFile(resolve(corruptDirectory, 'Data/settings.json'), corruptBytes)
  profileDirectory = await createTemporaryDirectory('vexa-tauri-webview2-corrupt-')
  debuggerPort = await getFreePort()
  launchApp()
  page = await connectAppPage()
  await page.locator('.toast-message').filter({ hasText: '設定檔格式錯誤' }).waitFor({ state: 'visible' })
  assert.equal(await page.locator('.app-shell').getAttribute('data-font-size'), 'large')
  await page.locator('.theme-toggle').click()
  await page.locator('.toast-message').filter({ hasText: '無法保存至 Data/settings.json' }).waitFor({ state: 'visible' })
  assert.equal(await readFile(resolve(corruptDirectory, 'Data/settings.json'), 'utf8'), corruptBytes)
  console.log('PASS 損壞設定：Release EXE 正常啟動、提示後使用預設值，且不覆寫原始檔')

  await browser.close().catch(() => undefined)
  browser = undefined
  await stopApp()
  const corruptSessionDirectory = await createTemporaryDirectory('vexa-corrupt-session-')
  executable = resolve(corruptSessionDirectory, 'vexa.exe')
  await copyFile(sourceExecutable, executable)
  await mkdir(resolve(corruptSessionDirectory, 'Data'))
  const corruptSessionBytes = '{broken session json'
  await writeFile(resolve(corruptSessionDirectory, 'Data/session.json'), corruptSessionBytes)
  profileDirectory = await createTemporaryDirectory('vexa-tauri-webview2-corrupt-session-')
  debuggerPort = await getFreePort()
  launchApp()
  page = await connectAppPage()
  await page.locator('.toast-message').filter({ hasText: 'Data/session.json' }).waitFor({ state: 'visible' })
  await page.waitForTimeout(1_300)
  assert.equal(await page.locator('.tabs .tab').count(), 1)
  assert.equal(await readFile(resolve(corruptSessionDirectory, 'Data/session.json'), 'utf8'), corruptSessionBytes)
  console.log('PASS 損壞 Session：Release EXE 安全恢復空白 Tab，並保留原始 JSON')

  await browser.close().catch(() => undefined)
  browser = undefined
  await stopApp()
  const blockedDirectory = await createTemporaryDirectory('vexa-readonly-settings-')
  executable = resolve(blockedDirectory, 'vexa.exe')
  await copyFile(sourceExecutable, executable)
  aclDirectory = resolve(blockedDirectory, 'Data')
  await mkdir(aclDirectory)
  aclIdentity = (await execFileAsync('whoami', [], { windowsHide: true })).stdout.trim()
  await execFileAsync('icacls', [aclDirectory, '/deny', `${aclIdentity}:(OI)(CI)(W)`], { windowsHide: true })
  profileDirectory = await createTemporaryDirectory('vexa-tauri-webview2-readonly-')
  debuggerPort = await getFreePort()
  launchApp()
  page = await connectAppPage()
  await page.locator('.theme-toggle').click()
  await page.locator('.toast-message').filter({ hasText: '無法保存至 Data/settings.json' }).waitFor({ state: 'visible' })
  let permissionSettingsCreated = true
  try { await readFile(resolve(blockedDirectory, 'Data/settings.json')) } catch (error) { if (error?.code === 'ENOENT' || error?.code === 'EACCES') permissionSettingsCreated = false; else throw error }
  assert.equal(permissionSettingsCreated, false)
  console.log('PASS 唯讀 Portable 目錄：UI 正常啟動與套用主題，並提示設定無法保存')
} catch (error) {
  console.error('FAIL Tauri IPC smoke test')
  console.error(error)
  if (stderr) console.error(stderr)
  process.exitCode = 1
} finally {
  await browser?.close().catch(() => undefined)
  await stopApp()
  if (aclDirectory && aclIdentity) {
    try { await execFileAsync('icacls', [aclDirectory, '/remove:d', aclIdentity], { windowsHide: true }) } catch { /* Cleanup retries after the app has exited. */ }
  }
  appServer.closeAllConnections()
  await new Promise((resolveClose) => appServer.close(() => resolveClose()))
  for (const directory of temporaryDirectories) {
    for (let attempt = 0; ; attempt += 1) {
      try {
        await rm(directory, { recursive: true, force: true, maxRetries: 8, retryDelay: 250 })
        break
      } catch (error) {
        if (attempt >= 5 || !['EBUSY', 'EPERM', 'ENOTEMPTY'].includes(error?.code)) throw error
        await new Promise((resolveDelay) => setTimeout(resolveDelay, 500))
      }
    }
  }
}
