import { useQuery } from "@tanstack/react-query";
import { Download, Inbox, Loader2, TriangleAlert } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { formatBytes } from "@/lib/download";
import {
  armIosDrop,
  type DropBatch,
  type DropFile,
  INBOX_PATH,
  IOS_DROP_BUNDLE,
  type IosDropCapture,
  iosDropBundle,
  publishIosDrop,
  readInbox,
} from "@/lib/drop-inbox";
import { fetchDeviceFile, listDeviceFiles, streamDeviceFile } from "@/lib/screen/session";
import { trpcClient } from "@/lib/trpc";

/** Slow enough to be free, fast enough that a share feels like it arrives. */
const POLL_MS = 2000;

/**
 * Take files off a device that were shared into YARD Drop.
 *
 * The companion app never learns a provider URL or a YARD credential — it
 * writes a batch to Downloads or app Documents and this dialog reads it over
 * the same authenticated file plane as the file browser, so every download is
 * already in the audit log. Nothing new crosses the coordinator.
 */
export function DeviceDropDialog({
  deviceId,
  platform,
  active,
  open,
  onOpenChange,
}: {
  deviceId: string;
  platform?: string;
  active: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  // Survives the polls, not the dialog: reading a manifest is an audited file
  // pull, and a batch never changes once it is complete.
  const seen = useRef(new Map<string, DropBatch>());
  const ios = platform === "ios";
  const [capture, setCapture] = useState<IosDropCapture | null>(null);
  const [publishing, setPublishing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [bundleId, setBundleId] = useState(IOS_DROP_BUNDLE);
  const epoch = useRef(0);
  const root = ios
    ? `app:${bundleId}:/Documents/YARD Drop/Inbox/${capture?.id ?? "unarmed"}`
    : INBOX_PATH;

  useEffect(() => {
    if (!active) {
      epoch.current++;
      setCapture(null);
      setPublishing(false);
      seen.current.clear();
    }
  }, [active]);

  const launch = (appId: string) => (args: string[]) =>
    trpcClient.device.launch.mutate({ deviceId, appId, args });

  const transfer = async (prepare: boolean) => {
    setBusy(true);
    const started = epoch.current;
    try {
      if (prepare) {
        const bundle = iosDropBundle(await trpcClient.device.apps.query({ deviceId }));
        if (started !== epoch.current) return;
        const armed = await armIosDrop(launch(bundle));
        if (started !== epoch.current) return;
        seen.current.clear();
        setBundleId(bundle);
        setCapture(armed);
        setPublishing(false);
      } else if (capture) {
        await publishIosDrop(launch(bundleId), capture);
        if (started !== epoch.current) return;
        setPublishing(true);
      }
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "The iPhone could not be prepared");
    } finally {
      setBusy(false);
    }
  };

  const inbox = useQuery({
    queryKey: ["device-drop", deviceId, root],
    queryFn: () =>
      readInbox(
        {
          list: (path) => listDeviceFiles(deviceId, path),
          read: (path) => fetchDeviceFile(deviceId, path),
        },
        seen.current,
        root,
      ),
    // Only while the dialog is open: `enabled` stops the interval too, so
    // closing is all the cleanup there is.
    enabled: open && active && (!ios || publishing),
    refetchInterval: POLL_MS,
    staleTime: 0,
    retry: false,
  });

  // A share is whatever the tester had on the phone, so it is sized like a
  // build artifact rather than a screenshot: the browser streams it to disk
  // instead of assembling it in a tab. That leaves nothing to report here —
  // failing to mint a token is the only part still ours.
  const download = async (file: DropFile) => {
    try {
      await streamDeviceFile(deviceId, file.path);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Download failed");
    }
  };

  const batches = active && (!ios || publishing) ? (inbox.data ?? []) : [];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Receive shared files</DialogTitle>
          <DialogDescription>
            {ios
              ? "Prepare the iPhone, return to the app you are testing and share files to YARD - Device Farm. Choose Send to YARD browser, then select Finish transfer here."
              : "On the device, share the files and choose YARD - Device Farm, then Send to YARD browser. They appear here."}{" "}
            Downloads are recorded in the audit log.
          </DialogDescription>
        </DialogHeader>

        {ios && (
          <div className="space-y-2">
            <div className="flex gap-2">
              <Button disabled={!active || busy} onClick={() => void transfer(true)}>
                {busy && <Loader2 className="mr-2 size-4 animate-spin" />}
                {capture ? "Prepare another transfer" : "Prepare iPhone"}
              </Button>
              {capture && (
                <Button
                  variant="outline"
                  disabled={!active || busy}
                  onClick={() => void transfer(false)}
                >
                  Finish transfer
                </Button>
              )}
            </div>
            <p className="text-muted-foreground text-sm">
              {capture
                ? "Wait for Ready to receive on the iPhone before sharing. Transfers expire after 10 minutes. After a browser refresh, prepare a new transfer."
                : "YARD Drop must be installed on the iPhone before the reservation begins."}
            </p>
          </div>
        )}

        <div className="min-w-0 rounded-md border">
          <div className="max-h-[22rem] min-h-[12rem] overflow-y-auto">
            {ios && !publishing && (
              <div className="flex h-48 items-center justify-center px-6 text-center text-muted-foreground text-sm">
                {capture
                  ? "Share your files on the iPhone, then select Finish transfer."
                  : "Prepare the iPhone to start a transfer."}
              </div>
            )}
            {inbox.isPending && (!ios || publishing) && (
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
                            onClick={() => void download(file)}
                          >
                            <Download className="size-4" />
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
