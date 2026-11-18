import { userEvent, within } from "storybook/test"

/** Portalled menus and dialogs land on `document.body`, outside the story canvas. */
export function pageOf(canvasElement: HTMLElement) {
  return within(canvasElement.ownerDocument.body)
}

/** Opens the top bar's actions drawer for the thread on screen. */
export async function openSessionActions(canvasElement: HTMLElement, label: string) {
  const canvas = within(canvasElement)
  await userEvent.click(
    await canvas.findByRole("button", { name: `Actions for ${label}` }, { timeout: 5000 }),
  )
  return within(
    await pageOf(canvasElement).findByRole("dialog", { name: label }),
  )
}

/** Opens a nav row's own-actions menu. The trigger is hover-only in CSS. */
export async function openRowMenu(canvasElement: HTMLElement, label: string) {
  const canvas = within(canvasElement)
  await userEvent.click(
    await canvas.findByRole(
      "button",
      { name: `More actions for ${label}` },
      { timeout: 5000 },
    ),
  )
  return pageOf(canvasElement)
}

/** Opens the built-in Environments plugin settings from its row menu. */
export async function openEnvironmentsSettings(canvasElement: HTMLElement) {
  const canvas = within(canvasElement)
  await userEvent.click(
    await canvas.findByRole("button", { name: /More actions for Environments/ }, { timeout: 5000 }),
  )
  const page = pageOf(canvasElement)
  await userEvent.click(await page.findByRole("menuitem", { name: /Settings/ }))
  return within(await page.findByRole("dialog"))
}
