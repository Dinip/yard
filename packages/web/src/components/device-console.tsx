import type { Display } from "@farm/protocol";
import {
  Camera,
  ClipboardCopy,
  ClipboardPaste,
  ExternalLink,
  RotateCw,
  Upload,
} from "lucide-react";
import { type DragEvent, useRef, useState } from "react";
import { toast } from "sonner";
import { DeviceScreen } from "@/components/device-screen";
import { Button } from "@/components/ui/button";
import { useDeviceSession } from "@/hooks/use-device-session";
import { fetchScreenshot, installApp } from "@/lib/screen/session";
import { cn } from "@/lib/utils";

/**
 * The whole control surface: screen, input, clipboard, screenshot, rotate and
 * drag-and-drop install. Shared verbatim by `/devices/:id` and the chrome-free
 * popout, which is why it takes no layout of its own beyond a column.
 */
export function DeviceConsole({
  deviceId,
  active,
  className,
  showPopout = true,
}: {
  deviceId: string;
  /** False when the device is not reserved by this user — no session is opened. */
  active: boolean;
  className?: string;
  showPopout?: boolean;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const session = useDeviceSession(deviceId, canvasRef, active);
  const [install, setInstall] = useState<{ name: string; fraction: number } | null>(null);
  const [dragging, setDragging] = useState(false);

  const upload = async (file: File) => {
    setInstall({ name: file.name, fraction: 0 });
    try {
      await installApp(deviceId, file, (fraction) =>
        setInstall((current) => (current ? { ...current, fraction } : current)),
      );
      toast.success(`Installed ${file.name}`);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Install failed");
    } finally {
      setInstall(null);
    }
  };

  const onDrop = (event: DragEvent) => {
    event.preventDefault();
    setDragging(false);
    const file = event.dataTransfer.files[0];
    if (!file) return;
    if (!active) {
      toast.error("Reserve the device before installing");
      return;
    }
    void upload(file);
  };

  const screenshot = async () => {
    try {
      const blob = await fetchScreenshot(deviceId);
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = `${deviceId}-${Date.now()}.png`;
      link.click();
      URL.revokeObjectURL(url);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Screenshot failed");
    }
  };

  const rotate = () => {
    const next = (((session.display?.rotation ?? 0) + 90) % 360) as number;
    session.send({ type: "rotate", degrees: next });
  };

  const readDeviceClipboard = async () => {
    try {
      const text = await session.readClipboard();
      if (!text) {
        toast.message("The device's clipboard is empty");
        return;
      }
      await navigator.clipboard.writeText(text);
      toast.success("Copied the device's clipboard");
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Clipboard read failed");
    }
  };

  const writeDeviceClipboard = async () => {
    try {
      const text = await navigator.clipboard.readText();
      session.send({ type: "clipboard.set", text });
      toast.success("Sent to the device's clipboard");
    } catch {
      toast.error("This browser would not share its clipboard");
    }
  };

  return (
    // `application` is the honest role for a remote-control surface: the canvas
    // consumes keystrokes itself, and the container is a drop target whose
    // keyboard equivalent is the Install button in the toolbar.
    <div
      role="application"
      aria-label="Device screen and controls"
      className={cn("flex min-h-0 flex-col gap-3", className)}
      onDragOver={(event) => {
        event.preventDefault();
        setDragging(true);
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={onDrop}
    >
      <div className="relative flex min-h-0 flex-1">
        <DeviceScreen session={session} canvasRef={canvasRef} className="flex-1" />
        {dragging && (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center rounded-md border-2 border-primary border-dashed bg-background/80 text-sm">
            <span className="flex items-center gap-2">
              <Upload className="size-4" /> Drop an APK or IPA to install
            </span>
          </div>
        )}
      </div>

      {install && (
        <div className="space-y-1">
          <div className="flex justify-between text-muted-foreground text-xs">
            <span className="truncate">{install.name}</span>
            <span>
              {install.fraction >= 1 ? "installing…" : `${Math.round(install.fraction * 100)}%`}
            </span>
          </div>
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
            <div
              className="h-full bg-primary transition-[width]"
              style={{ width: `${Math.round(install.fraction * 100)}%` }}
            />
          </div>
        </div>
      )}

      <div className="flex flex-wrap items-center gap-2">
        <Button variant="outline" size="sm" disabled={!active} onClick={rotate}>
          <RotateCw className="size-4" /> Rotate
        </Button>
        <Button variant="outline" size="sm" disabled={!active} onClick={screenshot}>
          <Camera className="size-4" /> Screenshot
        </Button>
        <Button variant="outline" size="sm" disabled={!active} onClick={readDeviceClipboard}>
          <ClipboardCopy className="size-4" /> Copy from device
        </Button>
        <Button variant="outline" size="sm" disabled={!active} onClick={writeDeviceClipboard}>
          <ClipboardPaste className="size-4" /> Paste to device
        </Button>
        <InstallButton disabled={!active} onFile={upload} />
        {showPopout && (
          <Button
            variant="outline"
            size="sm"
            disabled={!active}
            onClick={() => openPopout(deviceId, session.display)}
          >
            <ExternalLink className="size-4" /> Pop out
          </Button>
        )}
      </div>
    </div>
  );
}

function InstallButton({
  disabled,
  onFile,
}: {
  disabled: boolean;
  onFile: (file: File) => void | Promise<void>;
}) {
  const input = useRef<HTMLInputElement | null>(null);
  return (
    <>
      <input
        ref={input}
        type="file"
        accept=".apk,.ipa"
        className="hidden"
        onChange={(event) => {
          const file = event.target.files?.[0];
          event.target.value = "";
          if (file) void onFile(file);
        }}
      />
      <Button
        variant="outline"
        size="sm"
        disabled={disabled}
        onClick={() => input.current?.click()}
      >
        <Upload className="size-4" /> Install
      </Button>
    </>
  );
}

/**
 * The popout joins the *same* reservation as this tab — reservations are per
 * user+device — and the provider fans video out per viewer, so this needs no
 * protocol support beyond a second session.
 */
function openPopout(deviceId: string, display: Display | null) {
  const scale = display?.scale ?? 1;
  const logicalWidth = display?.width ? display.width / scale : 400;
  const logicalHeight = display?.height ? display.height / scale : 800;
  // Leave room for the toolbar and the window's own chrome.
  const maxHeight = Math.round(window.screen.availHeight * 0.9) - 120;
  const factor = Math.min(1, maxHeight / logicalHeight);
  const width = Math.round(logicalWidth * factor) + 32;
  const height = Math.round(logicalHeight * factor) + 120;
  window.open(
    `/devices/${deviceId}/popout`,
    `farm-${deviceId}`,
    `width=${width},height=${height},menubar=no,toolbar=no,location=no,status=no`,
  );
}
