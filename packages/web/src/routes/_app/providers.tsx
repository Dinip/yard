import { createFileRoute, redirect } from "@tanstack/react-router";

/**
 * The read-only provider list used to live here, next to an admin page showing
 * the same table with buttons on it. Only the admin one survives.
 *
 * This stays behind as a redirect rather than being deleted outright: the path
 * was linked from the nav for long enough to be bookmarked, and a bare "Not
 * found" is a poor answer to a link that used to work. A non-admin following it
 * lands on `/admin/providers`, whose own guard sends them to `/devices`.
 */
export const Route = createFileRoute("/_app/providers")({
  beforeLoad: () => {
    throw redirect({ to: "/admin/providers" });
  },
});
