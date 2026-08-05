import type { Display } from "@farm/protocol";
import {
  Camera,
  ClipboardCopy,
  ClipboardPaste,
  ExternalLink,
  RotateCw,
  Upload,
} from "lucide-react";
import { type DragEvent, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { DeviceScreen } from "@/components/device-screen";
import { Button } from "@/components/ui/button";
import { useDeviceSession } from "@/hooks/use-device-session";
import { fetchScreenshot, installApp } from "@/lib/screen/session";
import { cn } from "@/lib/utils";

/** How long the overlay bar lingers after the pointer stops moving. */
const OVERLAY_LINGER = 2_500;

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
  controls = "toolbar",
}: {
  deviceId: string;
  /** False when the device is not reserved by this user — no session is opened. */
  active: boolean;
  className?: string;
  showPopout?: boolean;
  /**
   * `"overlay"` floats the same actions over the video instead of below it.
   * The popout is a window sized to a screen; controls competing with it for
   * space is the whole reason it felt like half a feature.
   */
  controls?: "toolbar" | "overlay";
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const session = useDeviceSession(deviceId, canvasRef, active);
  const [install, setInstall] = useState<{ name: string; fraction: number } | null>(null);
  const [dragging, setDragging] = useState(false);
  const [controlsVisible, setControlsVisible] = useState(true);
  const hideTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  /** Overlay only: reveal on movement, fade back out after a pause. */
  const wake = () => {
    if (controls !== "overlay") return;
    setControlsVisible(true);
    if (hideTimer.current) clearTimeout(hideTimer.current);
    hideTimer.current = setTimeout(() => setControlsVisible(false), OVERLAY_LINGER);
  };

  useEffect(() => {
    if (controls !== "overlay") return;
    const timer = setTimeout(() => setControlsVisible(false), OVERLAY_LINGER);
    return () => {
      clearTimeout(timer);
      if (hideTimer.current) clearTimeout(hideTimer.current);
    };
  }, [controls]);

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

  // One list, two renderings: the toolbar labels them, the overlay does not.
  // A control that exists in one place and not the other is how the popout
  // ends up a lesser window than the page.
  const actions = [
    { key: "rotate", label: "Rotate", icon: RotateCw, run: rotate },
    { key: "screenshot", label: "Screenshot", icon: Camera, run: screenshot },
    {
      key: "copy",
      label: "Copy from device",
      icon: ClipboardCopy,
      run: readDeviceClipboard,
    },
    {
      key: "paste",
      label: "Paste to device",
      icon: ClipboardPaste,
      run: writeDeviceClipboard,
    },
  ];

  return (
    // `application` is the honest role for a remote-control surface: the canvas
    // consumes keystrokes itself, and the container is a drop target whose
    // keyboard equivalent is the Install button in the toolbar.
    <div
      role="application"
      aria-label="Device screen and controls"
      className={cn("flex min-h-0 flex-col", controls === "overlay" ? "gap-0" : "gap-3", className)}
      onDragOver={(event) => {
        event.preventDefault();
        setDragging(true);
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={onDrop}
      onPointerMove={wake}
    >
      <div className="relative flex min-h-0 flex-1">
        <DeviceScreen session={session} canvasRef={canvasRef} className="flex-1" />

        {controls === "overlay" && (
          <div
            // `pointer-events-none` while faded, or an invisible bar would eat
            // taps meant for the bottom of the screen.
            className={cn(
              "absolute inset-x-0 bottom-0 flex justify-center pb-3 transition-opacity duration-200 focus-within:pointer-events-auto focus-within:opacity-100 hover:pointer-events-auto hover:opacity-100",
              controlsVisible ? "opacity-100" : "pointer-events-none opacity-0",
            )}
          >
            <div className="flex items-center gap-1 rounded-full border bg-background/85 px-2 py-1 shadow-sm backdrop-blur">
              {actions.map((action) => (
                <Button
                  key={action.key}
                  variant="ghost"
                  size="icon"
                  disabled={!active}
                  title={action.label}
                  aria-label={action.label}
                  onClick={action.run}
                >
                  <action.icon className="size-4" />
                </Button>
              ))}
              <InstallButton disabled={!active} onFile={upload} iconOnly />
            </div>
          </div>
        )}

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

      {controls === "toolbar" && (
        <div className="flex flex-wrap items-center gap-2">
          {actions.map((action) => (
            <Button
              key={action.key}
              variant="outline"
              size="sm"
              disabled={!active}
              onClick={action.run}
            >
              <action.icon className="size-4" /> {action.label}
            </Button>
          ))}
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
      )}
    </div>
  );
}

function InstallButton({
  disabled,
  onFile,
  iconOnly = false,
}: {
  disabled: boolean;
  onFile: (file: File) => void | Promise<void>;
  iconOnly?: boolean;
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
        variant={iconOnly ? "ghost" : "outline"}
        size={iconOnly ? "icon" : "sm"}
        disabled={disabled}
        title="Install"
        aria-label="Install"
        onClick={() => input.current?.click()}
      >
        <Upload className="size-4" /> {!iconOnly && "Install"}
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
  // Only the window's own chrome: the popout's controls float over the video
  // rather than sitting under it, so the screen gets the whole window.
  const CHROME = 40;
  const maxHeight = Math.round(window.screen.availHeight * 0.9) - CHROME;
  const factor = Math.min(1, maxHeight / logicalHeight);
  const width = Math.round(logicalWidth * factor);
  const height = Math.round(logicalHeight * factor) + CHROME;
  window.open(
    `/devices/${deviceId}/popout`,
    `farm-${deviceId}`,
    `width=${width},height=${height},menubar=no,toolbar=no,location=no,status=no`,
  );
}
