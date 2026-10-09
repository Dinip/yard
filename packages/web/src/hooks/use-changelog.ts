import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getRouteApi } from "@tanstack/react-router";
import { trpc, trpcClient } from "@/lib/trpc";

export function useChangelog(version: string) {
  const { user } = getRouteApi("/_app").useRouteContext();
  const qc = useQueryClient();
  // Accounts sharing a browser must not share their read status in the cache.
  const queryKey = ["changelog-viewed", user.id, version];
  const viewed = useQuery({
    queryKey,
    queryFn: () => trpcClient.user.changelog.viewed.query({ version }),
    refetchInterval: 60_000,
  });
  const markViewed = useMutation(
    trpc.user.changelog.markViewed.mutationOptions({
      onSuccess: () => qc.setQueryData(queryKey, { viewed: true }),
    }),
  );
  return { viewed, markViewed };
}
