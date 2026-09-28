import { expect, type Page } from '@playwright/test'
import { test } from './helpers'

const PAGE = '/docs/getting-started/introduction'

const editor = (page: Page) => page.locator('[data-input]')
const status = (page: Page) => page.locator('[data-status]')

// fill() writes .value straight through and the keydown handler never runs, so every edit here goes through real keys.
async function clear(page: Page) {
  const input = editor(page)
  await input.focus()
  await input.press('ControlOrMeta+a')
  await input.press('Backspace')
}

test.beforeEach(async ({ page }) => {
  await page.goto(PAGE)
  await expect(editor(page)).toBeVisible()
})

test('auto-pairs an opener and skips the closer', async ({ page }) => {
  const input = editor(page)
  await clear(page)

  await input.press('(')
  await expect(input).toHaveValue('()')

  await input.press('"')
  await expect(input).toHaveValue('("")')

  await input.press('"')
  await input.press(')')
  await expect(input).toHaveValue('("")')
})

test('inherits indentation and opens a level after a colon', async ({ page }) => {
  const input = editor(page)
  await clear(page)

  await input.pressSequentially('if True:')
  await input.press('Enter')
  await expect(input).toHaveValue('if True:\n  ')
})

test('indents and outdents a selection with Tab', async ({ page }) => {
  const input = editor(page)
  await clear(page)

  await input.pressSequentially('a')
  await input.press('Enter')
  await input.pressSequentially('b')
  await input.press('ControlOrMeta+a')

  await input.press('Tab')
  await expect(input).toHaveValue('  a\n  b')

  await input.press('Shift+Tab')
  await expect(input).toHaveValue('a\nb')
})

test('highlights the source with Shiki', async ({ page }) => {
  const tokens = page.locator('[data-view] span')

  await expect(tokens.first()).toBeAttached()
  expect(await tokens.count()).toBeGreaterThan(1)
})

test('scrolls the highlight with the source', async ({ page }) => {
  const input = editor(page)
  await clear(page)

  for (let i = 0; i < 8; i++) {
    await input.pressSequentially(`print(${i})`)
    await input.press('Enter')
  }

  const view = await input.evaluate((el: HTMLTextAreaElement) => {
    const overlay = el.previousElementSibling!
    return { scroll: el.parentElement!.scrollTop, drift: overlay.getBoundingClientRect().top - el.getBoundingClientRect().top }
  })

  expect(view.scroll).toBeGreaterThan(0)
  expect(view.drift).toBe(0)
})

test('runs an edit that keeps the documented output and reports the elapsed time', async ({ page }) => {
  const input = editor(page)
  const expected = await page.locator('[data-playground]').getAttribute('data-expected')

  // A trailing comment changes the source and nothing it prints.
  await input.focus()
  await input.evaluate((el: HTMLTextAreaElement) => el.setSelectionRange(el.value.length, el.value.length))
  await input.pressSequentially('  # edited')

  // The output is server rendered, so only the elapsed time tells us the run actually finished.
  await expect(status(page)).toHaveText(/^Output, \d+(\.\d+)?(ms|s)$/, { timeout: 30000 })
  await expect(page.locator('[data-output]')).toHaveText(expected!.trim())
})

test('runs an edit once typing pauses and reports a mismatch against the documented output', async ({ page }) => {
  const input = editor(page)
  await clear(page)
  await input.pressSequentially('print("something else"')

  await expect(status(page)).toHaveText('Output, differs', { timeout: 30000 })
})

test.describe('on a touch screen', () => {
  test.use({ hasTouch: true })

  // A phone keeps the hover a tap leaves, which once held the menu open.
  test('closes the file menu on a second tap of its button', async ({ page }) => {
    const button = page.locator('[data-files] > button')
    const pick = page.locator('[data-pick="edge.json"]')

    await button.tap()
    await expect(pick).toBeVisible()

    await button.tap()
    await expect(pick).toBeHidden()
  })
})
