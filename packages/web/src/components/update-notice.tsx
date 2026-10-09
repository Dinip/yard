import { Link } from "@tanstack/react-router";
import { Sparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useChangelog } from "@/hooks/use-changelog";
import { VERSION } from "@/lib/build-info";
import { currentRelease } from "@/lib/releases";

export function UpdateNotice() {
  const { viewed } = useChangelog(VERSION);
  if (!currentRelease || viewed.data?.viewed !== false) return null;

  return (
    <aside
      aria-label="App update"
      aria-live="polite"
      className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b bg-accent/50 px-6 py-2"
    >
      <Sparkles aria-hidden="true" className="size-4 text-primary" />
      <p className="flex-1 text-sm">
        <span className="font-medium">What's new in YARD v{VERSION}?</span>{" "}
        <span className="text-muted-foreground">See the latest changes.</span>
      </p>
      <Button asChild variant="outline" size="sm">
        <Link to="/changelog" search={{}}>
          View changelog
        </Link>
      </Button>
    </aside>
  );
}
