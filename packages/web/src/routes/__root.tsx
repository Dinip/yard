import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, Outlet } from "@tanstack/react-router";
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
    <>
      <Outlet />
      <Toaster richColors position="bottom-right" />
    </>
  );
}
