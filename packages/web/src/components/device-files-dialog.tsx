import type { FileEntry } from "@farm/protocol";
import { useQuery } from "@tanstack/react-query";
import { CornerLeftUp, Download, File, FileQuestion, Folder, Loader2 } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { formatBytes, saveBlob } from "@/lib/download";
import { fetchDeviceFile, listDeviceFiles } from "@/lib/screen/session";

/**
 * The breadcrumb, for a path the browser otherwise treats as opaque.
 *
 * iOS has two disjoint trees behind one browse — the media domain and a
 * per-app container — so its paths carry a scheme (`media:/DCIM`,
 * `app:<bundle>:/Documents`). That is the provider's business, not something to
 * show a person, so it is unwrapped here rather than being decoded anywhere
 * that matters.
 */
function displayPath(path: string | undefined): string {
  if (!path) return "…";
  if (path === "/") return "This device";
  if (path.startsWith("media:")) return `Media ${path.slice("media:".length)}`;
  const app = /^app:([^:]+):(.*)$/.exec(path);
  if (app) return `${app[1]} ${app[2]}`;
  return path;
}

/**
 * Browse the device's filesystem and take a file off it.
 *
 * Read-only, deliberately. Writing to a device's filesystem is a separate
 * decision with its own audit weight, and `install` already covers the one file
 * anybody needs to put on a device.
 *
 * The listing and the bytes both come from the provider directly, like the
 * screenshot and the install upload — the coordinator only mints the token, and
 * hears about the download afterwards so it can write the audit row.
 */
export function DeviceFilesDialog({
  deviceId,
  platform,
  open,
  onOpenChange,
}: {
  deviceId: string;
  platform: string | undefined;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  /** Undefined means "wherever the backend opens" — the browser never guesses. */
  const [path, setPath] = useState<string | undefined>(undefined);
  const [downloading, setDownloading] = useState<string | null>(null);

  const listing = useQuery({
    queryKey: ["device-files", deviceId, path ?? null],
    queryFn: () => listDeviceFiles(deviceId, path),
    enabled: open,
    // A device's filesystem changes under us constantly; a cached listing from
    // five minutes ago is worse than a spinner.
    staleTime: 0,
    retry: false,
  });

  const download = async (entry: FileEntry) => {
    setDownloading(entry.path);
    try {
      saveBlob(await fetchDeviceFile(deviceId, entry.path), entry.name);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Download failed");
    } finally {
      setDownloading(null);
    }
  };

  const data = listing.data;

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // Reopening starts at the root rather than wherever the last browse
        // ended: a stale deep path on a device that has since been re-imaged
        // opens on an error for no reason the user can see.
        if (!next) setPath(undefined);
        onOpenChange(next);
      }}
    >
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Device files</DialogTitle>
          <DialogDescription>
            {platform === "ios"
              ? // An iPhone has no single filesystem, so saying what these two
                // entries are saves a tester hunting for a "real" root that
                // does not exist.
                "Media holds photos, downloads and books. The apps below it are those that share files — that is where anything saved with “Save to Files” lands."
              : "Browse the device and download a file. Downloads are recorded in the audit log."}
          </DialogDescription>
        </DialogHeader>

        {/* `min-w-0` is load-bearing: `DialogContent` is a grid, and a grid
            child's default `min-width: auto` refuses to shrink below its
            content — so one long path pushed the whole dialog off the window
            rather than being clipped inside it. */}
        <div className="min-w-0 rounded-md border">
          {/* Scrolls rather than truncating, because the *end* of a path is the
              part worth reading and an ellipsis eats exactly that. */}
          <div className="overflow-x-auto whitespace-nowrap border-b bg-muted/40 px-3 py-2 font-mono text-muted-foreground text-xs">
            {displayPath(data?.path ?? path)}
          </div>

          <div className="max-h-[22rem] min-h-[12rem] overflow-y-auto">
            {listing.isPending && (
              <div className="flex h-48 items-center justify-center text-muted-foreground text-sm">
                <Loader2 className="mr-2 size-4 animate-spin" /> Reading the device…
              </div>
            )}

            {listing.isError && (
              <div className="flex h-48 flex-col items-center justify-center gap-3 px-6 text-center">
                {/* The device's own words — "Permission denied" is the whole
                    answer, and rewording it would hide which path said no. */}
                <p className="text-destructive text-sm">
                  {listing.error instanceof Error ? listing.error.message : "Could not read that"}
                </p>
                {path !== undefined && (
                  <Button variant="outline" size="sm" onClick={() => setPath(undefined)}>
                    Back to the start
                  </Button>
                )}
              </div>
            )}

            {data && (
              <ul className="divide-y">
                {data.parent !== null && (
                  <li>
                    <button
                      type="button"
                      className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-muted/50"
                      onClick={() => setPath(data.parent ?? undefined)}
                    >
                      <CornerLeftUp className="size-4 shrink-0 text-muted-foreground" />
                      <span className="text-muted-foreground">..</span>
                    </button>
                  </li>
                )}

                {data.entries.length === 0 && (
                  <li className="px-3 py-8 text-center text-muted-foreground text-sm">
                    This directory is empty
                  </li>
                )}

                {data.entries.map((entry) => (
                  <li key={entry.path}>
                    {entry.kind === "directory" ? (
                      <button
                        type="button"
                        className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-muted/50"
                        onClick={() => setPath(entry.path)}
                      >
                        <Folder className="size-4 shrink-0 text-muted-foreground" />
                        <span className="min-w-0 flex-1 truncate">{entry.name}</span>
                      </button>
                    ) : (
                      <div className="flex items-center gap-2 px-3 py-2 text-sm">
                        {/* A symlink or a device node is listed but not offered
                            for download — the backend says which, so the UI
                            does not have to guess from the name. */}
                        {entry.kind === "file" ? (
                          <File className="size-4 shrink-0 text-muted-foreground" />
                        ) : (
                          <FileQuestion className="size-4 shrink-0 text-muted-foreground" />
                        )}
                        <span className="min-w-0 flex-1 truncate">{entry.name}</span>
                        <span className="shrink-0 tabular-nums text-muted-foreground text-xs">
                          {formatBytes(entry.size)}
                        </span>
                        {entry.kind === "file" && (
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-7 shrink-0"
                            title={`Download ${entry.name}`}
                            aria-label={`Download ${entry.name}`}
                            disabled={downloading !== null}
                            onClick={() => void download(entry)}
                          >
                            {downloading === entry.path ? (
                              <Loader2 className="size-4 animate-spin" />
                            ) : (
                              <Download className="size-4" />
                            )}
                          </Button>
                        )}
                      </div>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
