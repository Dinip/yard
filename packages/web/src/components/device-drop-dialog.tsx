import { useQuery } from "@tanstack/react-query";
import { Download, Inbox, Loader2, TriangleAlert } from "lucide-react";
import { useRef, useState } from "react";
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
import { type DropBatch, type DropFile, readInbox } from "@/lib/drop-inbox";
import { fetchDeviceFile, listDeviceFiles } from "@/lib/screen/session";

/** Slow enough to be free, fast enough that a share feels like it arrives. */
const POLL_MS = 2000;

/**
 * Take files off a device that were shared into YARD Drop.
 *
 * The companion app never learns a provider URL or a YARD credential — it
 * writes a batch to the device's Downloads and this dialog reads it back over
 * the same authenticated file plane as the file browser, so every download is
 * already in the audit log. Nothing new crosses the coordinator.
 */
export function DeviceDropDialog({
  deviceId,
  open,
  onOpenChange,
}: {
  deviceId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [downloading, setDownloading] = useState<string | null>(null);
  // Survives the polls, not the dialog: reading a manifest is an audited file
  // pull, and a batch never changes once it is complete.
  const seen = useRef(new Map<string, DropBatch>());

  const inbox = useQuery({
    queryKey: ["device-drop", deviceId],
    queryFn: () =>
      readInbox(
        {
          list: (path) => listDeviceFiles(deviceId, path),
          read: (path) => fetchDeviceFile(deviceId, path),
        },
        seen.current,
      ),
    // Only while the dialog is open: `enabled` stops the interval too, so
    // closing is all the cleanup there is.
    enabled: open,
    refetchInterval: POLL_MS,
    staleTime: 0,
    retry: false,
  });

  const download = async (file: DropFile) => {
    setDownloading(file.path);
    try {
      saveBlob(await fetchDeviceFile(deviceId, file.path), file.name);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Download failed");
    } finally {
      setDownloading(null);
    }
  };

  const batches = inbox.data ?? [];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Receive shared files</DialogTitle>
          <DialogDescription>
            On the device, share the files and choose YARD Drop, then “Send to YARD browser”. They
            appear here. Downloads are recorded in the audit log.
          </DialogDescription>
        </DialogHeader>

        <div className="min-w-0 rounded-md border">
          <div className="max-h-[22rem] min-h-[12rem] overflow-y-auto">
            {inbox.isPending && (
              <div className="flex h-48 items-center justify-center text-muted-foreground text-sm">
                <Loader2 className="mr-2 size-4 animate-spin" /> Reading the device…
              </div>
            )}

            {inbox.isSuccess && batches.length === 0 && (
              <div className="flex h-48 flex-col items-center justify-center gap-2 px-6 text-center text-muted-foreground">
                <Inbox className="size-6" />
                <p className="text-sm">Waiting for a share from the device</p>
              </div>
            )}

            <ul className="divide-y">
              {batches.map((batch) => (
                <li key={batch.path} className="px-3 py-3">
                  <p className="mb-2 text-muted-foreground text-xs">
                    {batch.createdAt ? new Date(batch.createdAt).toLocaleString() : batch.id}
                  </p>

                  {batch.error ? (
                    <p className="flex items-start gap-2 text-destructive text-sm">
                      <TriangleAlert className="mt-0.5 size-4 shrink-0" />
                      {batch.error}
                    </p>
                  ) : (
                    <ul className="space-y-1">
                      {batch.files.map((file) => (
                        <li key={file.path} className="flex items-center gap-2 text-sm">
                          <span className="min-w-0 flex-1 truncate">{file.name}</span>
                          <span className="shrink-0 tabular-nums text-muted-foreground text-xs">
                            {formatBytes(file.size)}
                          </span>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-7 shrink-0"
                            title={`Download ${file.name}`}
                            aria-label={`Download ${file.name}`}
                            disabled={downloading !== null}
                            onClick={() => void download(file)}
                          >
                            {downloading === file.path ? (
                              <Loader2 className="size-4 animate-spin" />
                            ) : (
                              <Download className="size-4" />
                            )}
                          </Button>
                        </li>
                      ))}
                    </ul>
                  )}
                </li>
              ))}
            </ul>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
