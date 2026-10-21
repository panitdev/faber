import { createRootRoute, Outlet } from "@tanstack/react-router"
import { SquircleNoScript } from "@squircle-js/react"

import { AmbientBackground } from "@charlotte-ui/labs/ambient-background"
import { AppAuth } from "@/components/shell/app-auth"
import { AppShell } from "@/components/shell/app-shell"

export const Route = createRootRoute({
  component: RootLayout,
})

function RootLayout() {
  return (
    <>
      <SquircleNoScript />
      <AmbientBackground />
      <AppAuth>
        <AppShell>
          <Outlet />
        </AppShell>
      </AppAuth>
    </>
  )
}
