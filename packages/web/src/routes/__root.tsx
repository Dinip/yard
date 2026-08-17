import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, Outlet } from "@tanstack/react-router";
import { ThemeProvider } from "next-themes";
import { Toaster } from "@/components/ui/sonner";
import type { trpc } from "@/lib/trpc";

export interface RouterContext {
  queryClient: QueryClient;
  trpc: typeof trpc;
}

export const Route = createRootRouteWithContext<RouterContext>()({
  component: RootComponent,
  notFoundComponent: () => (
    <div className="flex min-h-svh items-center justify-center text-muted-foreground">
      Not found.
    </div>
  ),
});

function RootComponent() {
  return (
    // `system` is the default so a first visit follows the browser, and keeps
    // following it live. The storage key is shared with the pre-paint script in
    // index.html, and — because localStorage is per origin — it is also what
    // syncs the popout window to the tab that opened it.
    <ThemeProvider
      attribute="class"
      defaultTheme="system"
      enableSystem
      disableTransitionOnChange
      storageKey="theme"
    >
      <Outlet />
      <Toaster richColors position="bottom-right" />
    </ThemeProvider>
  );
}
