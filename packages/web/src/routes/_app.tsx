import { createFileRoute, Outlet, redirect } from "@tanstack/react-router";
import { AppShell } from "@/components/app-shell";
import { authClient } from "@/lib/auth-client";

/**
 * Pathless layout guarding everything that needs a session. `beforeLoad` runs
 * before any child loader, so no authenticated query is ever fired signed-out.
 */
export const Route = createFileRoute("/_app")({
  beforeLoad: async ({ location }) => {
    const { data } = await authClient.getSession();
    if (!data?.session) {
      throw redirect({ to: "/login", search: { redirect: location.href } });
    }
    return { user: data.user };
  },
  component: () => (
    <AppShell>
      <Outlet />
    </AppShell>
  ),
});
