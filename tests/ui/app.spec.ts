import { expect, test, type Page } from '@playwright/test'
import { normalizeHttpUrl } from '../../src/url'

type TestBridge = {
  sent: Array<{ tabId: string; executionId: string; url: string; method: string }>
  invoked: string[]
  canceled: string[]
  pending: Map<string, (result: unknown) => void>
  cancelCounts: Map<string, number>
  pwned: boolean
  savedSettings: Array<{ version: 1; appearance: { theme: 'system' | 'light' | 'dark'; fontSize: 'small' | 'normal' | 'large' | 'xlarge'; sourceFontSize: number | null } }>
}

function contrastRatio(foreground: string, background: string) {
  const luminance = (hex: string) => {
    const channels = hex.match(/[a-f\d]{2}/gi)!.map((part) => Number.parseInt(part, 16) / 255)
      .map((channel) => channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4)
    return 0.2126 * channels[0]! + 0.7152 * channels[1]! + 0.0722 * channels[2]!
  }
  const a = luminance(foreground)
  const b = luminance(background)
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05)
}

declare global {
  interface Window {
    __VEXA_TEST__: TestBridge
    __TAURI_INTERNALS__: { invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown> }
  }
}

async function installTauriMock(page: Page) {
  await page.addInitScript(() => {
    const bridge: TestBridge = {
      sent: [],
      invoked: [],
      canceled: [],
      pending: new Map(),
      cancelCounts: new Map(),
      pwned: false,
      savedSettings: [],
    }
    const encode = (text: string) => {
      const bytes = new TextEncoder().encode(text)
      let binary = ''
      for (const byte of bytes) binary += String.fromCharCode(byte)
      return btoa(binary)
    }
    const response = (status: number, text: string, contentType = 'text/plain; charset=utf-8', extraHeaders: Array<{ name: string; value: string }> = []) => ({
      status,
      statusText: ({ 200: 'OK', 302: 'Found', 400: 'Bad Request', 404: 'Not Found', 500: 'Internal Server Error' } as Record<number, string>)[status] ?? 'Status',
      durationMs: 3,
      responseSize: new TextEncoder().encode(text).length,
      truncated: false,
      headers: [{ name: 'content-type', value: contentType }, ...extraHeaders],
      bodyBase64: encode(text),
    })
    const finish = (request: { tabId: string; executionId: string }, result: Record<string, unknown>) => ({
      tabId: request.tabId,
      executionId: request.executionId,
      ...result,
    })
    window.__VEXA_TEST__ = bridge
    window.__TAURI_INTERNALS__ = {
      invoke: async (command: string, args: Record<string, unknown> = {}) => {
        bridge.invoked.push(command)
        if (command === 'load_ui_settings') {
          const legacy = args.legacy as { theme?: string; fontSize?: string; sourceFontSize?: number } | undefined
          const existing = sessionStorage.getItem('__vexa_settings')
          if (existing) return { settings: JSON.parse(existing), migrated: false, warning: null }
          const settings = { version: 1 as const, historyLimit: 1000, appearance: {
            theme: (legacy?.theme === 'light' || legacy?.theme === 'dark' ? legacy.theme : 'system') as 'system' | 'light' | 'dark',
            fontSize: (['small', 'normal', 'large', 'xlarge'].includes(legacy?.fontSize ?? '') ? legacy!.fontSize : 'large') as 'small' | 'normal' | 'large' | 'xlarge',
            sourceFontSize: Number.isInteger(legacy?.sourceFontSize) && legacy!.sourceFontSize! >= 12 && legacy!.sourceFontSize! <= 32 ? legacy!.sourceFontSize! : null,
          } }
          const migrated = Boolean(legacy?.theme || legacy?.fontSize || legacy?.sourceFontSize)
          if (migrated) sessionStorage.setItem('__vexa_settings', JSON.stringify(settings))
          return { settings, migrated, warning: null }
        }
        if (command === 'save_ui_settings') {
          const settings = args.settings as typeof bridge.savedSettings[number]
          bridge.savedSettings.push(settings)
          sessionStorage.setItem('__vexa_settings', JSON.stringify(settings))
          return undefined
        }
        if (command === 'load_history') {
          return { value: { version: 1, entries: JSON.parse(sessionStorage.getItem('__vexa_history') ?? '[]') }, warning: null }
        }
        if (command === 'record_history') {
          const request = args.input as Record<string, unknown>
          const entries = JSON.parse(sessionStorage.getItem('__vexa_history') ?? '[]')
          entries.unshift({ id: crypto.randomUUID(), executedAt: new Date().toISOString(), request: { ...request, needsSensitiveReview: false, bodyContentSaved: Boolean(request.saveBodyContent) } })
          sessionStorage.setItem('__vexa_history', JSON.stringify(entries.slice(0, 1000)))
          return undefined
        }
        if (command === 'clear_history') {
          sessionStorage.setItem('__vexa_history', '[]')
          return undefined
        }
        if (command === 'load_session') {
          return { value: JSON.parse(sessionStorage.getItem('__vexa_session') ?? '{"version":1,"tabs":[],"activeTabId":""}'), warning: null }
        }
        if (command === 'save_session') {
          sessionStorage.setItem('__vexa_session', JSON.stringify({ version: 1, tabs: args.tabs, activeTabId: args.activeTabId }))
          return undefined
        }
        if (command === 'send_request') {
          const request = args.request as { tabId: string; executionId: string; url: string; method: string }
          bridge.sent.push({ tabId: request.tabId, executionId: request.executionId, url: request.url, method: request.method })
          const path = new URL(request.url).pathname
          if (path === '/cancel' && (bridge.cancelCounts.get(path) ?? 0) === 0) {
            return new Promise((resolve) => bridge.pending.set(request.executionId, resolve))
          }
          await new Promise((resolve) => window.setTimeout(resolve, path === '/slow' ? 700 : 0))
          if (path === '/json') return finish(request, { response: response(200, '{"name":"測試","nested":{"ok":true},"items":[1,2]}', 'application/json') , error: null })
          if (path === '/long-text' || path === '/other-long-text') return finish(request, { response: response(200, Array.from({ length: 160 }, (_, index) => `line ${index + 1}: ${'可換行內容 '.repeat(12)}`).join('\n'), 'text/plain; charset=utf-8'), error: null })
          if (path === '/invalid-json') return finish(request, { response: response(200, '{ invalid', 'application/json'), error: null })
          if (path === '/empty-json') return finish(request, { response: response(200, '', 'application/json'), error: null })
          if (path === '/xml') return finish(request, { response: response(200, '<root><item>繁體</item></root>', 'application/xml'), error: null })
          if (path === '/html') return finish(request, { response: response(200, '<h1>Fixture</h1><script>parent.__VEXA_TEST__.pwned=true;parent.__TAURI_INTERNALS__.invoke("send_request",{})</script><img src="https://invalid.example/x.png" onerror="parent.__VEXA_TEST__.pwned=true">', 'text/html'), error: null })
          if (path === '/image') return finish(request, { response: { ...response(200, ''), headers: [{ name: 'content-type', value: 'image/png' }], bodyBase64: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jq8sAAAAASUVORK5CYII=' }, error: null })
          if (path === '/binary') return finish(request, { response: { ...response(200, ''), headers: [{ name: 'content-type', value: 'application/octet-stream' }], bodyBase64: 'AAEC/yo=' }, error: null })
          if (path === '/cookies') return finish(request, { response: response(200, 'ok', 'text/plain', [{ name: 'set-cookie', value: 'sid=fixture; HttpOnly' }]), error: null })
          if (path === '/error') return finish(request, { response: response(500, 'fixture error'), error: null })
          if (path.startsWith('/status/')) return finish(request, { response: response(Number(path.slice('/status/'.length)), 'status fixture'), error: null })
          if (path === '/fast') return finish(request, { response: response(200, 'fast result'), error: null })
          if (path === '/slow') return finish(request, { response: response(200, 'slow result'), error: null })
          return finish(request, { response: response(200, 'retry result'), error: null })
        }
        if (command === 'cancel_request') {
          const executionId = String(args.executionId)
          bridge.canceled.push(executionId)
          const resolve = bridge.pending.get(executionId)
          const entry = bridge.sent.find((request) => request.executionId === executionId)
          if (entry) bridge.cancelCounts.set(new URL(entry.url).pathname, (bridge.cancelCounts.get(new URL(entry.url).pathname) ?? 0) + 1)
          bridge.pending.delete(executionId)
          resolve?.({ ...finish({ tabId: entry?.tabId ?? '', executionId }, { response: null, error: { kind: 'canceled', message: 'Request 已取消。' } }) })
          return true
        }
        if (command === 'save_response') return undefined
        if (command === 'plugin:dialog|save') return null
        if (command === 'plugin:dialog|open') return null
        if (command === 'plugin:clipboard-manager|write_text') return undefined
        throw new Error(`Unexpected mocked IPC command: ${command}`)
      },
    }
  })
}

test.beforeEach(async ({ page }) => {
  await installTauriMock(page)
  await page.goto('/')
})

test('normalizes HTTP URLs and rejects unsupported or malformed input', () => {
  const cases: Array<[string, string]> = [
    ['www.google.com', 'https://www.google.com'],
    ['api.example.com/test?x=1&y=two', 'https://api.example.com/test?x=1&y=two'],
    ['api.example.com:8443/test?q=a%2Fb', 'https://api.example.com:8443/test?q=a%2Fb'],
    ['localhost:5001/api/health', 'http://localhost:5001/api/health'],
    ['127.0.0.1:8080', 'http://127.0.0.1:8080'],
    ['[::1]:3000/test', 'http://[::1]:3000/test'],
    ['http://example.com', 'http://example.com'],
    ['https://example.com', 'https://example.com'],
  ]
  for (const [input, expected] of cases) expect(normalizeHttpUrl(input)).toEqual({ ok: true, value: expected })
  expect(normalizeHttpUrl('ftp://example.com')).toMatchObject({ ok: false, message: expect.stringContaining('不支援') })
  expect(normalizeHttpUrl('   ')).toMatchObject({ ok: false, message: expect.stringContaining('必填') })
  expect(normalizeHttpUrl('https:///missing-host')).toMatchObject({ ok: false })
  expect(normalizeHttpUrl('example.com:70000')).toMatchObject({ ok: false })
})

test('keeps the icon-only brand, theme contrast, scalable text and JSON-backed font preference', async ({ page }) => {
  const main = page.locator('.app-shell')
  await expect(page.locator('.brand')).toHaveAttribute('aria-label', 'Vexa')
  await expect(page.locator('.brand')).not.toContainText('Vexa')
  await expect(page.locator('.brand-mark')).toHaveJSProperty('naturalWidth', 64)
  await expect(page.locator('.tabs')).toBeVisible()

  await page.locator('.theme-toggle').click()
  await expect(main).toHaveAttribute('data-theme', 'light')
  await expect(main).toHaveAttribute('data-font-size', 'large')
  const lightColors = await main.evaluate((element) => ({
    background: getComputedStyle(element).getPropertyValue('--app-bg').trim(),
    panel: getComputedStyle(element).getPropertyValue('--panel').trim(),
    input: getComputedStyle(element).getPropertyValue('--input-bg').trim(),
    text: getComputedStyle(element).getPropertyValue('--text').trim(),
    secondary: getComputedStyle(element).getPropertyValue('--text-secondary').trim(),
    muted: getComputedStyle(element).getPropertyValue('--muted').trim(),
  }))
  expect(lightColors.background).toBe('#f8fafc')
  expect(lightColors.text).toBe('#1e293b')
  expect(lightColors.secondary).toBe('#475569')
  expect(lightColors.muted).toBe('#64748b')
  expect(contrastRatio(lightColors.text, lightColors.background)).toBeGreaterThanOrEqual(4.5)
  expect(contrastRatio(lightColors.secondary, lightColors.panel)).toBeGreaterThanOrEqual(4.5)
  expect(contrastRatio(lightColors.muted, lightColors.input)).toBeGreaterThanOrEqual(4.5)

  await page.locator('.theme-toggle').click()
  await expect(main).toHaveAttribute('data-theme', 'dark')
  const darkColors = await main.evaluate((element) => ({
    background: getComputedStyle(element).getPropertyValue('--app-bg').trim(),
    panel: getComputedStyle(element).getPropertyValue('--panel').trim(),
    input: getComputedStyle(element).getPropertyValue('--input-bg').trim(),
    text: getComputedStyle(element).getPropertyValue('--text').trim(),
    secondary: getComputedStyle(element).getPropertyValue('--text-secondary').trim(),
    muted: getComputedStyle(element).getPropertyValue('--muted').trim(),
    button: getComputedStyle(element.querySelector('.send-button')!).backgroundColor,
  }))
  expect(darkColors.background).toBe('#111318')
  expect(darkColors.text).toBe('#f1f5f9')
  expect(darkColors.muted).toBe('#a3afbe')
  expect(darkColors.button).toBe('rgb(59, 130, 246)')
  expect(contrastRatio(darkColors.text, darkColors.background)).toBeGreaterThanOrEqual(4.5)
  expect(contrastRatio(darkColors.secondary, darkColors.panel)).toBeGreaterThanOrEqual(4.5)
  expect(contrastRatio(darkColors.muted, darkColors.input)).toBeGreaterThanOrEqual(4.5)

  await page.getByLabel('Request URL').fill('http://mock/json')
  await page.getByRole('button', { name: /Send/ }).click()
  const toolbarLayout = await page.locator('.request-toolbar').evaluate((toolbar) => {
    const rect = (selector: string) => toolbar.querySelector(selector)!.getBoundingClientRect()
    const method = rect('.method-select')
    const url = rect('.url-input')
    const send = rect('.send-button')
    return { bounds: toolbar.getBoundingClientRect(), method, url, send }
  })
  expect(toolbarLayout.method.right).toBeLessThanOrEqual(toolbarLayout.url.left)
  expect(toolbarLayout.url.right).toBeLessThanOrEqual(toolbarLayout.send.left)
  expect(toolbarLayout.send.right).toBeLessThanOrEqual(toolbarLayout.bounds.right)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await page.setViewportSize({ width: 900, height: 620 })
  const code = page.locator('.response-code')
  const expectedPixels = [14, 16, 18, 20]
  await page.getByRole('button', { name: '字體大小設定' }).click()
  for (const [index, [label, size]] of ([['Small', 'small'], ['Normal', 'normal'], ['Large', 'large'], ['XLarge', 'xlarge']] as const).entries()) {
    await page.getByRole('button', { name: new RegExp(`^${label}`) }).click()
    await expect(main).toHaveAttribute('data-font-size', size)
    expect(await main.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(expectedPixels[index])
    expect(await code.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(expectedPixels[index])
    await page.getByRole('button', { name: '字體大小設定' }).click()
    const layout = await page.evaluate(() => {
      const rect = (selector: string) => document.querySelector(selector)!.getBoundingClientRect()
      const toolbar = rect('.request-toolbar')
      const url = rect('.url-input')
      const send = rect('.send-button')
      const menu = rect('.font-menu')
      const requestPane = rect('.request-pane')
      const responsePane = rect('.response-pane')
      const sectionTabs = rect('.section-tabs')
      const sectionButtons = [...document.querySelectorAll('.section-tabs button')].map((button) => button.getBoundingClientRect())
      return { width: window.innerWidth, height: window.innerHeight, pageWidth: document.documentElement.scrollWidth, toolbar, url, send, menu, requestPane, responsePane, sectionTabs, sectionButtons }
    })
    expect(layout.url.right).toBeLessThanOrEqual(layout.send.left)
    expect(layout.send.right).toBeLessThanOrEqual(layout.toolbar.right)
    expect(layout.menu.right).toBeLessThanOrEqual(layout.width)
    expect(layout.menu.bottom).toBeLessThanOrEqual(layout.height)
    expect(layout.pageWidth).toBeLessThanOrEqual(layout.width)
    expect(layout.requestPane.right).toBeLessThanOrEqual(layout.responsePane.left)
    expect(layout.sectionButtons.at(-1)!.right).toBeLessThanOrEqual(layout.sectionTabs.right)

    await page.getByRole('button', { name: '字體大小設定' }).click()
    for (const section of ['Params', 'Headers', 'Auth', 'Body']) {
      await page.locator('.section-tabs').getByRole('button', { name: section, exact: true }).click()
      if (section === 'Auth' || section === 'Body') await expect(page.locator('.request-scroll .control-select')).toBeVisible()
      const panesFit = await page.evaluate(() => [...document.querySelectorAll('.request-pane, .response-pane')].every((pane) => pane.scrollWidth <= pane.clientWidth))
      expect(panesFit).toBe(true)
    }
    await page.getByRole('button', { name: '字體大小設定' }).click()
  }
  await page.reload()
  await expect(page.locator('.app-shell')).toHaveAttribute('data-font-size', 'xlarge')
})

test('migrates legacy preferences once when the JSON settings file is absent', async ({ page }) => {
  await page.evaluate(() => localStorage.setItem('vexa.ui.fontSize', 'normal'))
  await page.evaluate(() => localStorage.setItem('vexa.ui.theme', 'dark'))
  await page.evaluate(() => localStorage.setItem('vexa.response.sourceFontSize', '17'))
  await page.reload()
  await expect(page.locator('.app-shell')).toHaveAttribute('data-font-size', 'normal')
  await expect(page.locator('.app-shell')).toHaveAttribute('data-theme', 'dark')
  await expect.poll(() => page.locator('.app-shell').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(16)
  expect(await page.evaluate(() => localStorage.getItem('vexa.ui.fontSize'))).toBeNull()
  expect(await page.evaluate(() => JSON.parse(sessionStorage.getItem('__vexa_settings') ?? '{}'))).toMatchObject({ appearance: { fontSize: 'normal', theme: 'dark', sourceFontSize: 17 } })
})

test('settings menu closes for outside pointer, Escape, tab changes, and after selection', async ({ page }) => {
  const toggle = page.getByRole('button', { name: '字體大小設定' })
  const menu = page.getByRole('group', { name: '字體大小' })
  await toggle.click()
  await expect(menu).toBeVisible()
  await page.locator('.font-menu-title').click()
  await expect(menu).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(menu).toHaveCount(0)
  await toggle.click()
  await page.locator('.request-pane h1').click()
  await expect(menu).toHaveCount(0)
  await toggle.click()
  await page.getByRole('button', { name: '新增 Tab' }).click()
  await expect(menu).toHaveCount(0)
  await toggle.click()
  await page.getByRole('button', { name: /^Normal/ }).click()
  await expect(menu).toHaveCount(0)
  await toggle.click()
  await expect(menu).toBeVisible()
  await toggle.click()
  await expect(menu).toHaveCount(0)
})

test('timeout values remain visible at all UI font sizes', async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 620 })
  const timeout = page.getByLabel('Timeout 毫秒')
  for (const [label, size, pixels] of [['Small', 'small', 14], ['Normal', 'normal', 16], ['Large', 'large', 18], ['XLarge', 'xlarge', 20]] as const) {
    await page.getByRole('button', { name: '字體大小設定' }).click()
    await page.getByRole('button', { name: new RegExp(`^${label}`) }).click()
    await expect(page.locator('.app-shell')).toHaveAttribute('data-font-size', size)
    for (const value of ['30000', '120000', '300000']) {
      await timeout.fill(value)
      const dimensions = await timeout.evaluate((input: HTMLInputElement) => {
        const canvas = document.createElement('canvas')
        const context = canvas.getContext('2d')!
        context.font = getComputedStyle(input).font
        const inputRect = input.getBoundingClientRect()
        const unitRect = document.querySelector('.timeout-unit')!.getBoundingClientRect()
        const labelRect = document.querySelector('.timeout-label')!.getBoundingClientRect()
        const titleRect = document.querySelector('.request-pane h1')!.getBoundingClientRect()
        return { text: context.measureText((input as HTMLInputElement).value).width, width: inputRect.width, unitGap: unitRect.left - inputRect.right, labelLeft: labelRect.left, titleRight: titleRect.right, pixels: Number.parseFloat(getComputedStyle(document.querySelector('.app-shell')!).fontSize) }
      })
      expect(dimensions.pixels).toBe(pixels)
      expect(dimensions.width).toBeGreaterThanOrEqual(dimensions.text + 4)
      expect(dimensions.unitGap).toBeGreaterThanOrEqual(3)
      expect(dimensions.labelLeft).toBeGreaterThan(dimensions.titleRight)
    }
  }
})

test('keeps the add-tab button after the last tab and anchors the top controls during overflow', async ({ page }) => {
  const wideControls = await page.evaluate(() => {
    const rect = (selector: string) => document.querySelector<HTMLElement>(selector)!.getBoundingClientRect()
    const local = rect('.local-indicator')
    const theme = rect('.theme-toggle')
    const settings = rect('.settings-toggle')
    return { localWidth: local.width, localRight: local.right, themeLeft: theme.left, themeRight: theme.right, settingsLeft: settings.left, settingsRight: settings.right, viewportWidth: innerWidth }
  })
  expect(wideControls.localWidth).toBeGreaterThan(0)
  expect(wideControls.localRight).toBeLessThanOrEqual(wideControls.themeLeft)
  expect(wideControls.themeRight).toBeLessThan(wideControls.settingsLeft)
  expect(wideControls.settingsRight).toBeLessThanOrEqual(wideControls.viewportWidth)

  await page.setViewportSize({ width: 900, height: 620 })
  const tabs = page.locator('.tabs')
  const addButton = page.getByRole('button', { name: '新增 Tab' })
  const layout = () => tabs.evaluate((navigation) => {
    const tabItems = [...navigation.querySelectorAll<HTMLElement>('.tab')]
    const lastTab = tabItems.at(-1)!.getBoundingClientRect()
    const add = navigation.querySelector<HTMLElement>('.new-tab')!.getBoundingClientRect()
    const nav = navigation.getBoundingClientRect()
    const theme = document.querySelector<HTMLElement>('.theme-toggle')!.getBoundingClientRect()
    const settings = document.querySelector<HTMLElement>('.settings-toggle')!.getBoundingClientRect()
    return {
      tabCount: tabItems.length,
      addLeft: add.left,
      lastTabRight: lastTab.right,
      addVisible: add.left >= nav.left && add.right <= nav.right,
      scrollWidth: navigation.scrollWidth,
      clientWidth: navigation.clientWidth,
      themeRight: theme.right,
      settingsLeft: settings.left,
      settingsRight: settings.right,
      viewportWidth: innerWidth,
    }
  })

  expect((await layout()).tabCount).toBe(1)
  expect((await layout()).addLeft).toBeGreaterThanOrEqual((await layout()).lastTabRight)
  await addButton.click()
  expect((await layout()).tabCount).toBe(2)
  expect((await layout()).addLeft).toBeGreaterThanOrEqual((await layout()).lastTabRight)

  for (let index = 0; index < 7; index++) await addButton.click()
  await expect.poll(async () => (await layout()).scrollWidth).toBeGreaterThan((await layout()).clientWidth)
  let dimensions = await layout()
  expect(dimensions.tabCount).toBe(9)
  expect(dimensions.addLeft).toBeGreaterThanOrEqual(dimensions.lastTabRight)
  expect(dimensions.addVisible).toBe(true)
  expect(dimensions.settingsRight).toBeLessThanOrEqual(dimensions.viewportWidth)
  expect(dimensions.settingsLeft).toBeGreaterThan(dimensions.themeRight)

  await tabs.locator('.tab').last().getByLabel('關閉 Tab').click()
  dimensions = await layout()
  expect(dimensions.tabCount).toBe(8)
  expect(dimensions.addLeft).toBeGreaterThanOrEqual(dimensions.lastTabRight)
  expect(dimensions.settingsRight).toBeLessThanOrEqual(dimensions.viewportWidth)

  const shortcutsHandled = await page.evaluate(() => {
    const addEvent = new KeyboardEvent('keydown', { key: 't', ctrlKey: true, bubbles: true, cancelable: true })
    const addCanceled = !window.dispatchEvent(addEvent)
    const closeEvent = new KeyboardEvent('keydown', { key: 'w', ctrlKey: true, bubbles: true, cancelable: true })
    const closeCanceled = !window.dispatchEvent(closeEvent)
    return { addCanceled, closeCanceled }
  })
  expect(shortcutsHandled).toEqual({ addCanceled: true, closeCanceled: true })
  await expect(tabs.locator('.tab')).toHaveCount(8)
})

test('compacts tab and pane headings without clipping at 900 by 620 across all font sizes', async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 620 })
  await page.getByLabel('Timeout 毫秒').fill('300000')
  await page.getByLabel('Request URL').fill('http://mock/json')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.status-pill')).toContainText('200 OK')
  await expect(page.locator('.response-time')).toBeVisible()
  await expect(page.getByLabel('複製 Response Body')).toBeVisible()
  await expect(page.getByLabel('另存 Response Body')).toBeVisible()

  const sizes = [
    ['Small', 'small'],
    ['Normal', 'normal'],
    ['Large', 'large'],
    ['XLarge', 'xlarge'],
  ] as const
  let largeMetrics: { tabBar: number; requestHeading: number; responseHeading: number } | undefined
  for (const [label, size] of sizes) {
    await page.getByRole('button', { name: '字體大小設定' }).click()
    await page.getByRole('button', { name: new RegExp(`^${label}`) }).click()
    await expect(page.locator('.app-shell')).toHaveAttribute('data-font-size', size)
    const metrics = await page.evaluate(() => {
      const rect = (selector: string) => document.querySelector<HTMLElement>(selector)!.getBoundingClientRect()
      const tabBar = rect('.tabbar')
      const requestHeading = rect('.request-pane .pane-heading')
      const responseHeading = rect('.response-pane .pane-heading')
      const requestTitle = rect('.request-pane h1')
      const responseTitle = rect('.response-pane h1')
      const timeout = rect('.timeout-label')
      const timeoutInput = rect('.timeout-label input')
      const timeoutUnit = rect('.timeout-unit')
      const responseToolbar = rect('.response-toolbar')
      const responseTime = rect('.response-time')
      const plus = rect('.new-tab')
      const close = rect('.tab-active .tab-close')
      const contained = (child: DOMRect, parent: DOMRect) => child.top >= parent.top && child.bottom <= parent.bottom
      return {
        tabBarHeight: tabBar.height,
        requestHeadingHeight: requestHeading.height,
        responseHeadingHeight: responseHeading.height,
        requestHeadingContentsFit: contained(requestTitle, requestHeading) && contained(timeout, requestHeading) && contained(timeoutInput, timeout) && contained(timeoutUnit, timeout),
        responseHeadingContentsFit: contained(responseTitle, responseHeading) && contained(responseToolbar, responseHeading) && contained(responseTime, responseToolbar),
        tabControlsFit: contained(plus, tabBar) && contained(close, tabBar),
        width: innerWidth,
        height: innerHeight,
        pageWidth: document.documentElement.scrollWidth,
        timeoutValue: (document.querySelector<HTMLInputElement>('.timeout-label input')!).value,
        timeoutUnit: document.querySelector('.timeout-unit')!.textContent?.trim(),
      }
    })

    expect(metrics.requestHeadingHeight).toBe(metrics.responseHeadingHeight)
    expect(metrics.requestHeadingContentsFit).toBe(true)
    expect(metrics.responseHeadingContentsFit).toBe(true)
    expect(metrics.tabControlsFit).toBe(true)
    expect(metrics.width).toBe(900)
    expect(metrics.height).toBe(620)
    expect(metrics.pageWidth).toBeLessThanOrEqual(metrics.width)
    expect(metrics.timeoutValue).toBe('300000')
    expect(metrics.timeoutUnit).toBe('ms')
    if (size === 'large') {
      largeMetrics = { tabBar: metrics.tabBarHeight, requestHeading: metrics.requestHeadingHeight, responseHeading: metrics.responseHeadingHeight }
    }
  }

  expect(largeMetrics).toBeDefined()
  expect(largeMetrics!.tabBar).toBeGreaterThanOrEqual(42)
  expect(largeMetrics!.tabBar).toBeLessThanOrEqual(46)
  expect(largeMetrics!.requestHeading).toBeGreaterThanOrEqual(36)
  expect(largeMetrics!.requestHeading).toBeLessThanOrEqual(40)

  const splitter = page.locator('.splitter')
  const before = await page.locator('.request-pane').evaluate((element) => element.getBoundingClientRect().width)
  const splitBounds = await splitter.boundingBox()
  expect(splitBounds).not.toBeNull()
  await page.mouse.move(splitBounds!.x + splitBounds!.width / 2, splitBounds!.y + 20)
  await page.mouse.down()
  await page.mouse.move(splitBounds!.x + splitBounds!.width / 2 + 45, splitBounds!.y + 20)
  await page.mouse.up()
  const after = await page.locator('.request-pane').evaluate((element) => element.getBoundingClientRect().width)
  expect(after).toBeGreaterThan(before + 20)

  await page.getByLabel('複製 Response Body').click()
  expect(await page.evaluate(() => window.__VEXA_TEST__.invoked)).toContain('plugin:clipboard-manager|write_text')
  await page.getByLabel('另存 Response Body').click()
  expect(await page.evaluate(() => window.__VEXA_TEST__.invoked)).toContain('plugin:dialog|save')
})

test('scales only Response Source with Ctrl+Wheel and persists its independent size', async ({ page }) => {
  test.setTimeout(30_000)
  const url = page.getByLabel('Request URL')
  await url.fill('http://mock/long-text')
  await page.getByRole('button', { name: /Send/ }).click()
  const source = page.locator('.response-code')
  await expect(source).toBeVisible()
  expect(await source.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(18)

  await page.getByRole('button', { name: '字體大小設定' }).click()
  await page.getByRole('button', { name: /^Small/ }).click()
  expect(await source.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(14)

  const scrollArea = page.locator('.body-scroll')
  await scrollArea.evaluate((element) => { element.scrollTop = 0 })
  await source.hover()
  await page.mouse.wheel(0, 360)
  await expect.poll(() => scrollArea.evaluate((element) => element.scrollTop)).toBeGreaterThan(0)
  await scrollArea.evaluate((element) => { element.scrollTop = 0 })
  expect(await page.evaluate(() => window.__VEXA_TEST__.savedSettings.at(-1)?.appearance.sourceFontSize ?? null)).toBeNull()

  const prevented = await source.evaluate((element) => {
    const event = new WheelEvent('wheel', { deltaY: 0, ctrlKey: true, bubbles: true, cancelable: true })
    const dispatched = element.dispatchEvent(event)
    return { dispatched, defaultPrevented: event.defaultPrevented }
  })
  expect(prevented).toEqual({ dispatched: false, defaultPrevented: true })
  await page.getByLabel('Request URL').evaluate((element) => element.dispatchEvent(new WheelEvent('wheel', { deltaY: -120, ctrlKey: true, bubbles: true, cancelable: true })))
  expect(await page.evaluate(() => window.__VEXA_TEST__.savedSettings.at(-1)?.appearance.sourceFontSize ?? null)).toBeNull()

  const initialMetrics = await page.evaluate(() => ({ width: innerWidth, pixelRatio: devicePixelRatio, viewportScale: visualViewport?.scale }))
  await page.getByLabel('Request URL').evaluate((element) => element.dispatchEvent(new WheelEvent('wheel', { deltaY: -120, ctrlKey: true, bubbles: true, cancelable: true })))
  expect(await source.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(14)
  expect(await page.evaluate(() => window.__VEXA_TEST__.savedSettings.at(-1)?.appearance.sourceFontSize ?? null)).toBeNull()
  expect(await page.evaluate(() => ({ width: innerWidth, pixelRatio: devicePixelRatio, viewportScale: visualViewport?.scale }))).toEqual(initialMetrics)

  await source.evaluate((element) => element.dispatchEvent(new WheelEvent('wheel', { deltaY: -120, ctrlKey: true, bubbles: true, cancelable: true })))
  await expect.poll(() => source.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(15)
  await expect.poll(() => page.evaluate(() => window.__VEXA_TEST__.savedSettings.at(-1)?.appearance.sourceFontSize)).toBe(15)
  expect(await page.evaluate(() => ({ width: innerWidth, pixelRatio: devicePixelRatio, viewportScale: visualViewport?.scale }))).toEqual(initialMetrics)

  await page.getByRole('button', { name: '字體大小設定' }).click()
  await page.getByRole('button', { name: /^XLarge/ }).click()
  expect(await page.locator('.app-shell').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(20)
  expect(await source.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(15)
  await page.getByRole('button', { name: '字體大小設定' }).click()

  await page.getByRole('button', { name: '新增 Tab' }).click()
  await url.fill('http://mock/other-long-text')
  await page.getByRole('button', { name: /Send/ }).click()
  const secondSource = page.locator('.response-code')
  await expect(secondSource).toBeVisible()
  expect(await secondSource.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(15)
  await page.getByRole('button', { name: /GET \/long-text/ }).click()
  expect(await page.locator('.response-code').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(15)

  await page.reload()
  await expect(page.locator('.app-shell')).toHaveAttribute('data-font-size', 'xlarge')
  await url.fill('http://mock/long-text')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.response-code')).toBeVisible()
  expect(await page.locator('.response-code').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(15)
  await expect.poll(() => page.evaluate(() => window.__VEXA_TEST__.savedSettings.at(-1)?.appearance.sourceFontSize)).toBe(15)

  const sourceAfterReload = page.locator('.response-code')
  await sourceAfterReload.evaluate((element) => {
    for (let index = 0; index < 20; index += 1) element.dispatchEvent(new WheelEvent('wheel', { deltaY: -120, ctrlKey: true, bubbles: true, cancelable: true }))
  })
  await expect.poll(() => sourceAfterReload.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(32)
  await expect.poll(() => page.evaluate(() => window.__VEXA_TEST__.savedSettings.at(-1)?.appearance.sourceFontSize)).toBe(32)
  await sourceAfterReload.evaluate((element) => element.dispatchEvent(new WheelEvent('wheel', { deltaY: -120, ctrlKey: true, bubbles: true, cancelable: true })))
  expect(await page.evaluate(() => window.__VEXA_TEST__.savedSettings.at(-1)?.appearance.sourceFontSize)).toBe(32)

  await sourceAfterReload.evaluate((element) => {
    for (let index = 0; index < 25; index += 1) element.dispatchEvent(new WheelEvent('wheel', { deltaY: 120, ctrlKey: true, bubbles: true, cancelable: true }))
  })
  await expect.poll(() => sourceAfterReload.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(12)
  await expect.poll(() => page.evaluate(() => window.__VEXA_TEST__.savedSettings.at(-1)?.appearance.sourceFontSize)).toBe(12)
  await page.getByRole('button', { name: '字體大小設定' }).click()
  await page.getByRole('button', { name: /^Normal/ }).click()
  expect(await page.locator('.app-shell').evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(16)
  expect(await sourceAfterReload.evaluate((element) => Number.parseFloat(getComputedStyle(element).fontSize))).toBe(12)
})

test('sends the normalized URL and keeps prior Response distinct from URL errors', async ({ page }) => {
  const url = page.getByLabel('Request URL')
  await url.fill('api.example.com/test?keep=%2F')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(url).toHaveValue('https://api.example.com/test?keep=%2F')
  await expect.poll(() => page.evaluate(() => window.__VEXA_TEST__.sent[0]?.url)).toBe('https://api.example.com/test?keep=%2F')
  await expect(page.locator('.status-pill')).toContainText('200 OK')
  await url.focus()
  expect(await url.evaluate((input) => getComputedStyle(input).boxShadow)).not.toBe('none')

  await url.fill('ftp://example.com')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.error-banner')).toContainText('不支援')
  await expect(page.locator('.error-context')).toContainText('前次回應')
  await expect(page.locator('.response-code')).toContainText('retry result')
  expect(await page.evaluate(() => window.__VEXA_TEST__.sent)).toHaveLength(1)
})

test('keeps HTTP status colors semantic for success, redirect, client and server errors', async ({ page }) => {
  for (const [status, className] of [[200, 'status-ok'], [302, 'status-redirect'], [404, 'status-client-error'], [500, 'status-server-error']] as const) {
    await page.getByLabel('Request URL').fill(`http://mock/status/${status}`)
    await page.getByRole('button', { name: /Send/ }).click()
    await expect(page.locator('.status-pill')).toHaveClass(new RegExp(className))
  }
})

test('formats JSON, renders a collapsible tree, and marks HTTP errors as responses', async ({ page }) => {
  await page.getByLabel('Request URL').fill('http://mock/json')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.status-pill')).toContainText('200 OK')
  await expect(page.locator('.response-code')).toContainText('"測試"')
  await page.getByRole('button', { name: 'Tree' }).click()
  await expect(page.locator('.json-node').first()).toContainText('測試')
  await page.getByRole('button', { name: '收合節點' }).first().click()
  await expect(page.locator('.json-key').filter({ hasText: '"name"' })).toHaveCount(0)
  await page.getByRole('button', { name: '展開節點' }).first().click()
  await expect(page.locator('.json-key').filter({ hasText: '"name"' })).toHaveCount(1)
  await page.getByLabel('Request URL').fill('http://mock/error')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.status-pill')).toContainText('500 Internal Server Error')
  await expect(page.locator('.response-code')).toContainText('fixture error')
})

test('switches JSON, XML, HTML and binary viewer modes safely', async ({ page }) => {
  await page.getByLabel('Request URL').fill('http://mock/invalid-json')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.response-code')).toContainText('{ invalid')
  await page.getByLabel('Request URL').fill('http://mock/empty-json')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.status-pill')).toContainText('200 OK')
  await page.getByLabel('Request URL').fill('http://mock/xml')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.response-code')).toContainText('繁體')
  await page.getByLabel('Request URL').fill('http://mock/html')
  await page.getByRole('button', { name: /Send/ }).click()
  await page.getByRole('button', { name: 'Preview' }).click()
  const preview = page.getByTitle('隔離的 HTML 預覽')
  await expect(preview).toHaveAttribute('sandbox', '')
  await expect.poll(() => preview.evaluate((iframe: HTMLIFrameElement) => iframe.srcdoc)).toContain("default-src 'none'")
  await expect.poll(() => page.evaluate(() => window.__VEXA_TEST__.pwned)).toBe(false)
  await page.getByLabel('Request URL').fill('http://mock/binary')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.getByText('無法預覽此二進位內容')).toBeVisible()
  await page.getByLabel('Request URL').fill('http://mock/image')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.getByAltText('Response 圖片')).toBeVisible()
})

test('keeps three tab responses isolated when requests finish out of order', async ({ page }) => {
  await page.getByLabel('Request URL').fill('http://mock/slow')
  await page.getByRole('button', { name: /Send/ }).click()
  const slowTab = page.getByRole('button', { name: /GET \/slow/ })
  await expect(slowTab.locator('.tab-spinner')).toBeVisible()

  await page.getByRole('button', { name: '新增 Tab' }).click()
  await page.getByLabel('Request URL').fill('http://mock/fast')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.response-code')).toContainText('fast result')

  await page.getByRole('button', { name: '新增 Tab' }).click()
  await page.getByLabel('Request URL').fill('http://mock/error')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.response-code')).toContainText('fixture error')

  await expect(slowTab.locator('.tab-spinner')).toBeHidden()
  await slowTab.click()
  await expect(page.locator('.response-code')).toContainText('slow result')
  await page.getByRole('button', { name: /GET \/fast/ }).click()
  await expect(page.locator('.response-code')).toContainText('fast result')
  await page.getByRole('button', { name: /GET \/error/ }).click()
  await expect(page.locator('.response-code')).toContainText('fixture error')
})

test('blocks duplicate sends and ignores a result after its running tab is closed', async ({ page }) => {
  await page.getByLabel('Request URL').fill('http://mock/slow')
  await page.getByRole('button', { name: /Send/ }).click()
  await page.keyboard.press('Control+Enter')
  await expect(page.locator('.tab-spinner')).toBeVisible()
  expect(await page.evaluate(() => window.__VEXA_TEST__.sent)).toHaveLength(1)

  await page.getByLabel('關閉 Tab', { exact: true }).click()
  await expect(page.locator('.tabs .tab')).toHaveCount(1)
  await expect(page.locator('.response-code')).toHaveCount(0)
  await expect.poll(() => page.evaluate(() => window.__VEXA_TEST__.canceled.length)).toBe(1)
  await expect(page.locator('.tab-spinner')).toHaveCount(0)
})

test('cancel clears loading and allows sending again', async ({ page }) => {
  await page.getByLabel('Request URL').fill('http://mock/cancel')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.getByRole('button', { name: /取消/ })).toBeVisible()
  await page.getByRole('button', { name: /取消/ }).click()
  await expect(page.locator('.response-meta-empty')).toContainText('Request 已取消')
  await expect(page.locator('.tab-spinner')).toBeHidden()
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.response-code')).toContainText('retry result')
  expect(await page.evaluate(() => window.__VEXA_TEST__.canceled)).toHaveLength(1)
})

test('request headers, cookies, search and text copy commands reach the UI', async ({ page }) => {
  await page.getByLabel('Request URL').fill('http://mock/cookies')
  await page.getByRole('button', { name: /Send/ }).click()
  await page.locator('.response-tabs').getByRole('button', { name: /Headers/ }).click()
  await expect(page.locator('.header-table')).toContainText('sid=fixture; HttpOnly')
  await page.locator('.response-tabs').getByRole('button', { name: 'Body' }).click()
  await expect(page.getByLabel('搜尋 Response Body')).toBeVisible()
  await page.getByLabel('搜尋 Response Body').fill('ok')
  await expect(page.locator('.response-code mark')).toContainText('ok')
  await page.getByLabel('複製 Response Body').click()
  expect(await page.evaluate(() => window.__VEXA_TEST__.invoked)).toContain('plugin:clipboard-manager|write_text')
})

test('tab strip arrows and Shift+Wheel scroll while the fixed controls remain visible at 900px', async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 620 })
  const add = page.getByRole('button', { name: '新增 Tab' })
  for (let index = 0; index < 11; index++) await add.click()
  const nav = page.locator('.tabs')
  const scrollLeft = () => nav.evaluate((element) => element.scrollLeft)
  await expect(page.getByRole('button', { name: '向左捲動 Tabs' })).toBeVisible()
  await expect(page.getByRole('button', { name: '向右捲動 Tabs' })).toBeVisible()
  await expect(page.locator('.local-indicator')).toBeVisible()
  const end = await scrollLeft()
  expect(end).toBeGreaterThan(0)
  await expect.poll(() => nav.evaluate((root) => {
    const tab = root.querySelector('.tab-active')!
    const item = tab.getBoundingClientRect(); const bounds = root.getBoundingClientRect()
    return item.left >= bounds.left && item.right <= bounds.right
  })).toBe(true)
  await page.getByRole('button', { name: '向左捲動 Tabs' }).click()
  await expect.poll(scrollLeft).toBeLessThan(end)
  const beforeRight = await scrollLeft()
  await page.getByRole('button', { name: '向右捲動 Tabs' }).click()
  await expect.poll(scrollLeft).toBeGreaterThan(beforeRight)
  const beforeWheel = await scrollLeft()
  await nav.dispatchEvent('wheel', { deltaY: 180, deltaX: 0, shiftKey: true, bubbles: true, cancelable: true })
  await expect.poll(scrollLeft).toBeGreaterThan(beforeWheel)
  await nav.locator('.tab').first().click()
  await expect.poll(() => nav.evaluate((root) => {
    const tab = root.querySelector('.tab')!
    const item = tab.getBoundingClientRect(); const bounds = root.getBoundingClientRect()
    return item.left >= bounds.left && item.right <= bounds.right
  })).toBe(true)
  const fixed = await page.evaluate(() => {
    const controls = document.querySelector('.top-controls')!.getBoundingClientRect()
    return { right: controls.right, width: controls.width, local: document.querySelector('.local-indicator')!.getBoundingClientRect().width }
  })
  expect(fixed.local).toBeGreaterThan(0)
  expect(fixed.right).toBeLessThanOrEqual(900)
})

test('URL Enter and Ctrl+Enter send, body Enter and IME composition do not', async ({ page }) => {
  const url = page.getByLabel('Request URL')
  await expect(page.locator('.send-button kbd')).toHaveCount(0)
  await url.fill('http://mock/fast')
  await url.press('Enter')
  await expect(page.locator('.response-code')).toContainText('fast result')
  expect(await page.evaluate(() => window.__VEXA_TEST__.sent)).toHaveLength(1)

  await page.getByRole('region', { name: 'Request 編輯器' }).getByRole('button', { name: 'Body', exact: true }).click()
  await page.locator('.body-type').selectOption('text')
  const body = page.getByLabel('Request Body 編輯器')
  await body.fill('line one')
  await body.press('Enter')
  expect(await page.evaluate(() => window.__VEXA_TEST__.sent)).toHaveLength(1)

  await url.fill('http://mock/retry')
  await url.press('Control+Enter')
  await expect(page.locator('.response-code')).toContainText('retry result')
  expect(await page.evaluate(() => window.__VEXA_TEST__.sent)).toHaveLength(2)

  await url.evaluate((input) => input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', isComposing: true, bubbles: true, cancelable: true })))
  expect(await page.evaluate(() => window.__VEXA_TEST__.sent)).toHaveLength(2)
})

test('History records repeated sends, opens a new unsent Tab, and clearing leaves Tabs open', async ({ page }) => {
  await page.getByLabel('Request URL').fill('http://mock/fast')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.status-pill')).toContainText('200 OK')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect.poll(() => page.evaluate(() => JSON.parse(sessionStorage.getItem('__vexa_history') ?? '[]').length)).toBe(2)

  await page.getByRole('button', { name: 'History' }).click()
  await expect(page.locator('.history-entry')).toHaveCount(2)
  const sendsBeforeOpen = await page.evaluate(() => window.__VEXA_TEST__.sent.length)
  const tabsBeforeOpen = await page.locator('.tabs .tab').count()
  await page.locator('.history-entry').first().click()
  await expect(page.locator('.tabs .tab')).toHaveCount(tabsBeforeOpen + 1)
  await expect(page.locator('.response-empty')).toBeVisible()
  expect(await page.evaluate(() => window.__VEXA_TEST__.sent.length)).toBe(sendsBeforeOpen)
  await page.locator('.history-entry').first().click()
  await expect(page.locator('.tabs .tab')).toHaveCount(tabsBeforeOpen + 1)

  page.once('dialog', (dialog) => dialog.accept())
  await page.getByRole('button', { name: '清除全部 History' }).click()
  await expect(page.locator('.history-entry')).toHaveCount(0)
  await expect(page.locator('.tabs .tab')).toHaveCount(tabsBeforeOpen + 1)
})

test('Session debounces Request edits, restores tab order and selection without Responses', async ({ page }) => {
  await page.getByLabel('Request URL').fill('http://mock/fast')
  await page.getByRole('button', { name: /Send/ }).click()
  await expect(page.locator('.status-pill')).toContainText('200 OK')
  await page.getByRole('button', { name: '新增 Tab' }).click()
  await page.getByLabel('Request URL').fill('http://mock/slow')
  await page.locator('.tabs .tab').first().click()
  await expect.poll(() => page.evaluate(() => JSON.parse(sessionStorage.getItem('__vexa_session') ?? '{"tabs":[]}').tabs.length)).toBe(2)
  await page.reload()
  await expect(page.locator('.tabs .tab')).toHaveCount(2)
  await expect(page.getByLabel('Request URL')).toHaveValue('http://mock/fast')
  await expect(page.locator('.response-empty')).toBeVisible()
  await page.locator('.tabs .tab').last().getByLabel('關閉 Tab').click()
  await expect.poll(() => page.evaluate(() => JSON.parse(sessionStorage.getItem('__vexa_session') ?? '{"tabs":[]}').tabs.length)).toBe(1)
  await page.reload()
  await expect(page.locator('.tabs .tab')).toHaveCount(1)
  await expect(page.getByLabel('Request URL')).toHaveValue('http://mock/fast')
})

test('History retention setting is saved as a portable preference', async ({ page }) => {
  await page.getByRole('button', { name: '字體大小設定' }).click()
  await page.getByLabel('History 筆數上限').selectOption('5000')
  await expect.poll(() => page.evaluate(() => JSON.parse(sessionStorage.getItem('__vexa_settings') ?? '{}').historyLimit)).toBe(5000)
})
