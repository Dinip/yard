import { QueryClientProvider } from "@tanstack/react-query";
import { createRouter, RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { queryClient, trpc } from "./lib/trpc.ts";
import { routeTree } from "./routeTree.gen.ts";
import "./styles.css";

const router = createRouter({
  routeTree,
  context: { queryClient, trpc },
  defaultPreload: "intent",
  // Queries own the caching; the router should not add a second layer of it.
  defaultPreloadStaleTime: 0,
  scrollRestoration: true,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

const rootEl = document.getElementById("root");
if (!rootEl) throw new Error("#root missing from index.html");

createRoot(rootEl).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
