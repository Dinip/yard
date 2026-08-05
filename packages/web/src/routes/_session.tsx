import { createFileRoute, Outlet, redirect } from "@tanstack/react-router";
import { authClient } from "@/lib/auth-client";

/**
 * Same auth guard as `_app`, without the shell. Windows opened for a single
 * device carry no navigation — the whole viewport is the device.
 */
export const Route = createFileRoute("/_session")({
  beforeLoad: async ({ location }) => {
    const { data } = await authClient.getSession();
    if (!data?.session) {
      throw redirect({ to: "/login", search: { redirect: location.href } });
    }
    return { user: data.user };
  },
  component: () => <Outlet />,
});
