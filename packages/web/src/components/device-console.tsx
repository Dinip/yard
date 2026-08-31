import {
  Camera,
  ChevronLeft,
  Circle,
  CircleStop,
  ClipboardCopy,
  ClipboardPaste,
  FolderOpen,
  House,
  type LucideIcon,
  MoreHorizontal,
  Power,
  RotateCw,
  Square,
  Upload,
  Volume1,
  Volume2,
} from "lucide-react";
import {
  type DragEvent,
  type PointerEvent as ReactPointerEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { toast } from "sonner";
import { DeviceFilesDialog } from "@/components/device-files-dialog";
import { DeviceScreen } from "@/components/device-screen";
import { SidePanel } from "@/components/side-panel";
import { Button } from "@/components/ui/button";
import { useDeviceSession } from "@/hooks/use-device-session";
import {
  type Corner,
  controlsAxis,
  cornerClasses,
  loadCorner,
  nearestCorner,
  saveCorner,
} from "@/lib/controls-corner";
import { fileStamp, saveBlob } from "@/lib/download";
import {
  extensionFor,
  isRecordingSupported,
  MAX_RECORDING_MS,
  ScreenRecorder,
} from "@/lib/screen/recorder";
import { fetchScreenshot, installApp } from "@/lib/screen/session";
import { loadPanelOpen, savePanelOpen } from "@/lib/side-panels";
import { cn } from "@/lib/utils";

/** How far the handle must travel before a click counts as a drag. */
const DRAG_SLOP = 4;

/**
 * The whole control surface: screen, hardware buttons, input, clipboard,
 * screenshot, rotate and drag-and-drop install. Shared verbatim by
 * `/devices/:id` and the chrome-free popout, which is why it takes no layout of
 * its own beyond a column.
 */
export function DeviceConsole({
  deviceId,
  platform,
  active,
  admin = false,
  className,
  controls = "rail",
  onRevoked,
}: {
  deviceId: string;
  /**
   * Which hardware buttons exist, and how to phrase what the file browser can
   * reach. iOS has a Home button and nothing else — no Back, no Recents.
   */
  platform?: string;
  /** False when the device is not reserved by this user — no session is opened. */
  active: boolean;
  admin?: boolean;
  className?: string;
  /**
   * `"overlay"` puts the same actions behind a corner handle over the video
   * instead of in a rail beside it. The popout is a window sized to a screen;
   * controls competing with it for space is the whole reason it felt like half
   * a feature.
   */
  controls?: "rail" | "overlay";
  /**
   * The reservation was withdrawn mid-session. The console has no route to
   * navigate to and no reservation id to explain it with, so the page that
   * owns both is told once, rather than watching session state itself.
   */
  onRevoked?: (reason?: string) => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const session = useDeviceSession(deviceId, canvasRef, active);

  const revokedRef = useRef(false);
  useEffect(() => {
    if (session.revoked && !revokedRef.current) {
      revokedRef.current = true;
      onRevoked?.(session.detail);
    }
    if (!session.revoked) revokedRef.current = false;
  }, [session.revoked, session.detail, onRevoked]);
  const [install, setInstall] = useState<{ name: string; fraction: number } | null>(null);
  const [dragging, setDragging] = useState(false);
  const [filesOpen, setFilesOpen] = useState(false);

  // The recorder itself lives in a ref — it is not render state — while
  // `elapsed` drives the label and is the only thing that needs to re-render.
  const recorderRef = useRef<ScreenRecorder | null>(null);
  const [elapsed, setElapsed] = useState<number | null>(null);
  const recording = elapsed !== null;
  // Overlay only: the corner handle is expanded while hovered or focused, and
  // `pinned` keeps it open for anyone who clicked rather than hovered.
  const [hovering, setHovering] = useState(false);
  const [pinned, setPinned] = useState(false);
  const controlsOpen = hovering || pinned;

  const screenRef = useRef<HTMLDivElement | null>(null);
  const [corner, setCorner] = useState<Corner>(loadCorner);
  // Not remembered, unlike the corner: this one belongs to the device, and it
  // changes under the user the moment the screen turns.
  const axis = controlsAxis({ frameSize: session.frameSize, display: session.display });
  /** Set only while a drag is in flight; the handle follows the pointer. */
  const [dragOffset, setDragOffset] = useState<{ dx: number; dy: number } | null>(null);
  const dragFrom = useRef<{ x: number; y: number; moved: boolean } | null>(null);

  const startDrag = (event: ReactPointerEvent<HTMLButtonElement>) => {
    event.currentTarget.setPointerCapture(event.pointerId);
    dragFrom.current = { x: event.clientX, y: event.clientY, moved: false };
  };

  const moveDrag = (event: ReactPointerEvent<HTMLButtonElement>) => {
    const from = dragFrom.current;
    if (!from) return;
    const dx = event.clientX - from.x;
    const dy = event.clientY - from.y;
    // A few pixels of slop, so a click with a shaky hand stays a click.
    if (!from.moved && Math.hypot(dx, dy) < DRAG_SLOP) return;
    from.moved = true;
    setDragOffset({ dx, dy });
  };

  const endDrag = (event: ReactPointerEvent<HTMLButtonElement>) => {
    const from = dragFrom.current;
    dragFrom.current = null;
    setDragOffset(null);
    if (!from) return;

    if (!from.moved) {
      setPinned((current) => !current);
      return;
    }

    const bounds = screenRef.current?.getBoundingClientRect();
    if (!bounds) return;
    const next = nearestCorner(
      { x: event.clientX - bounds.left, y: event.clientY - bounds.top },
      bounds,
    );
    setCorner(next);
    saveCorner(next);
  };

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
      saveBlob(await fetchScreenshot(deviceId), `${deviceId}_${fileStamp()}.png`);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Screenshot failed");
    }
  };

  /**
   * Finalises whatever is recording and hands over the file.
   *
   * Everything that can end a recording funnels through here — the button, the
   * two-minute cap, a revoked session, the tab standing down for a popout — so
   * there is exactly one path on which the file gets saved. A recording that
   * vanished because the window it was started in went quiet would be the worst
   * way for this feature to fail.
   */
  const finishRecording = useCallback(async () => {
    const recorder = recorderRef.current;
    recorderRef.current = null;
    setElapsed(null);
    if (!recorder) return;

    const { blob, mimeType, cappedOut } = await recorder.stop();
    if (blob.size === 0) {
      toast.error("The recording came back empty");
      return;
    }

    const name = `${deviceId}_${fileStamp()}.${extensionFor(mimeType)}`;
    saveBlob(blob, name);
    if (cappedOut) toast.message(`Stopped at ${MAX_RECORDING_MS / 60_000} minutes — saved ${name}`);
    else toast.success(`Saved ${name}`);
  }, [deviceId]);

  const toggleRecording = () => {
    if (recorderRef.current) {
      void finishRecording();
      return;
    }
    const canvas = canvasRef.current;
    if (!canvas) return;

    const recorder = ScreenRecorder.start(canvas);
    if (!recorder) {
      toast.error("Nothing has been painted yet — wait for the first frame");
      return;
    }
    recorderRef.current = recorder;
    setElapsed(0);
  };

  // The label's clock, and the only thing that notices the cap firing: the
  // recorder stops itself at two minutes, and this is what turns that into a
  // saved file and a settled button.
  useEffect(() => {
    if (!recording) return;
    const timer = setInterval(() => {
      const recorder = recorderRef.current;
      if (!recorder) return;
      if (recorder.elapsed() >= MAX_RECORDING_MS) void finishRecording();
      else setElapsed(recorder.elapsed());
    }, 500);
    return () => clearInterval(timer);
  }, [recording, finishRecording]);

  // A session that is going away takes the picture with it, so save first.
  // `active` covers the popout handover — this tab stands down when a popout
  // opens — and unmount covers the tab simply closing.
  useEffect(() => {
    if (!active && recorderRef.current) void finishRecording();
  }, [active, finishRecording]);

  useEffect(() => {
    return () => {
      if (recorderRef.current) void finishRecording();
    };
  }, [finishRecording]);

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

  /**
   * A hardware button, as a down/up pair.
   *
   * Both edges go out because the two backends want different halves: Android
   * injects a keycode per edge and would leave the key held without the up,
   * while iOS presses and releases on the down edge and discards the up. One
   * shape satisfies both, and neither backend needs to know a button was
   * clicked rather than typed.
   */
  const press = (key: string) => {
    session.send({ type: "key", key, down: true });
    session.send({ type: "key", key, down: false });
  };

  const installInput = useRef<HTMLInputElement | null>(null);

  const [railOpen, setRailOpen] = useState(() => loadPanelOpen("controls"));
  const toggleRail = () =>
    setRailOpen((open) => {
      savePanelOpen("controls", !open);
      return !open;
    });

  // One grouped list, two renderings: the rail labels the groups, the overlay
  // separates them. A control that exists in one place and not the other is how
  // the popout ends up a lesser window than the page.
  const sections: ConsoleSection[] = [
    {
      key: "device",
      label: "Device",
      actions: [
        // iOS has no hardware Back and no task switcher to reach this way.
        ...(platform === "android"
          ? [{ key: "back", label: "Back", icon: ChevronLeft, run: () => press("Back") }]
          : []),
        { key: "home", label: "Home", icon: House, run: () => press("Home") },
        ...(platform === "android"
          ? [{ key: "recents", label: "Recents", icon: Square, run: () => press("AppSwitch") }]
          : []),
        // Both backends already spoke these; nothing but the buttons is new.
        ...(admin
          ? [
              { key: "volume-up", label: "Volume up", icon: Volume2, run: () => press("VolumeUp") },
              {
                key: "volume-down",
                label: "Volume down",
                icon: Volume1,
                run: () => press("VolumeDown"),
              },
              { key: "power", label: "Power", icon: Power, run: () => press("Power") },
            ]
          : []),
      ],
    },
    {
      key: "screen",
      label: "Screen",
      actions: [
        { key: "rotate", label: "Rotate", icon: RotateCw, run: rotate },
        { key: "screenshot", label: "Screenshot", icon: Camera, run: screenshot },
        // Hidden rather than disabled where the browser cannot record at all: a
        // permanently greyed button is a worse answer than no button.
        ...(isRecordingSupported()
          ? [
              {
                key: "record",
                label: recording ? `Stop recording · ${formatElapsed(elapsed ?? 0)}` : "Record",
                icon: recording ? CircleStop : Circle,
                danger: recording,
                run: toggleRecording,
              },
            ]
          : []),
      ],
    },
    {
      key: "files",
      label: "Files",
      actions: [
        {
          key: "install",
          label: "Install",
          icon: Upload,
          run: () => installInput.current?.click(),
        },
        { key: "browse", label: "Files", icon: FolderOpen, run: () => setFilesOpen(true) },
        { key: "copy", label: "Copy from device", icon: ClipboardCopy, run: readDeviceClipboard },
        { key: "paste", label: "Paste to device", icon: ClipboardPaste, run: writeDeviceClipboard },
      ],
    },
  ];

  return (
    // `application` is the honest role for a remote-control surface: the canvas
    // consumes keystrokes itself, and the container is a drop target whose
    // keyboard equivalent is the Install button in the rail.
    <div
      role="application"
      aria-label="Device screen and controls"
      // A row, so the rail sits beside the screen rather than under it: a
      // portrait device leaves the horizontal space empty anyway, and every row
      // of controls below the screen is a row of screen nobody gets.
      className={cn("flex min-h-0 gap-3", className)}
      onDragOver={(event) => {
        event.preventDefault();
        setDragging(true);
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={onDrop}
    >
      {controls === "rail" && (
        <SidePanel
          side="left"
          open={railOpen}
          onToggle={toggleRail}
          title="Controls"
          width="w-48"
          // The same object as the Details panel on the other flank: same
          // border, same corners, same header. They do the same job.
          className="rounded-lg border"
        >
          <div role="toolbar" aria-label="Device controls" className="flex flex-col gap-4 p-2">
            {sections.map((section) => (
              <div key={section.key} className="flex flex-col gap-0.5">
                <span className="px-2 font-medium text-[10px] text-muted-foreground uppercase tracking-wide">
                  {section.label}
                </span>
                {/* One per row, labelled. The rail is the tall thin space beside
                    a portrait device — it has height to spend and no width worth
                    saving, so spelling the actions out costs nothing and beats
                    ten unlabelled icons. It scrolls if a viewport is short. */}
                {section.actions.map((action) => (
                  <ActionButton key={action.key} action={action} disabled={!active} labelled />
                ))}
              </div>
            ))}
          </div>
        </SidePanel>
      )}

      <div className="flex min-h-0 flex-1 flex-col gap-2">
        <div ref={screenRef} className="relative flex min-h-0 flex-1">
          <DeviceScreen session={session} canvasRef={canvasRef} className="flex-1" />

          {controls === "overlay" && (
            // Every corner is in something's way — the bottom edge is the home
            // indicator, the top corners are where iOS pulls control centre and
            // notifications from — so the handle is draggable and remembers where
            // it was put. It is inset from the corner either way, to leave the
            // extreme pixels to the device's own edge gestures.
            <div
              role="toolbar"
              aria-label="Device controls"
              // Handle and bar are one line — the bar unfolds *from* the handle
              // rather than beside it — and `cornerClasses` fixes the extent
              // across that line so expanding the bar cannot nudge the handle:
              // the pill is the larger of the two, and a centred line would
              // re-centre both the moment it appeared.
              className={cn(
                "absolute flex gap-1",
                cornerClasses(corner, axis),
                dragOffset && "z-10",
              )}
              style={
                dragOffset
                  ? { transform: `translate(${dragOffset.dx}px, ${dragOffset.dy}px)` }
                  : undefined
              }
              onPointerEnter={() => setHovering(true)}
              onPointerLeave={() => setHovering(false)}
              onFocus={() => setHovering(true)}
              onBlur={(event) => {
                // Only when focus left the group entirely, or tabbing from one
                // action to the next would collapse the bar mid-keyboard-use.
                if (!event.currentTarget.contains(event.relatedTarget)) setHovering(false);
              }}
            >
              {controlsOpen && !dragOffset && (
                // The bar unfolds along the axis the window has room in, which
                // is the one the device is not long in: a column beside a
                // portrait picture, matching the rail, and a row along the edge
                // of a landscape one. Ten icons across a portrait popout is the
                // direction that cannot fit, and ten down a landscape one is
                // the same mistake turned sideways. Icon-only either way,
                // unlike the rail, because this floats *over* the picture.
                <div
                  className={cn(
                    "flex items-center gap-3 rounded-2xl border bg-background/85 shadow-sm backdrop-blur",
                    // One line, scrolled rather than wrapped: wrapping would
                    // take the pill off the line it shares with the handle. The
                    // cap leaves room for the handle and both insets.
                    axis === "horizontal"
                      ? "max-w-[calc(100svw-6rem)] flex-row overflow-x-auto px-2 py-1"
                      : "max-h-[calc(100svh-6rem)] flex-col overflow-y-auto px-1 py-2",
                  )}
                >
                  {/* Proximity does the grouping — there is no room out here for
                      headings, and a divider drawn per group is more line than
                      a stack of ten buttons needs. */}
                  {sections.map((section) => (
                    <div
                      key={section.key}
                      className={cn(
                        "flex items-center gap-1",
                        axis === "horizontal" ? "flex-row" : "flex-col",
                      )}
                    >
                      {section.actions.map((action) => (
                        <ActionButton key={action.key} action={action} disabled={!active} />
                      ))}
                    </div>
                  ))}
                </div>
              )}
              <Button
                variant="ghost"
                size="icon"
                // Keyboard only (`detail === 0`); a mouse click is handled on
                // pointer-up, which is the only place that can tell a click from
                // the end of a drag.
                onClick={(event) => {
                  if (event.detail === 0) setPinned((current) => !current);
                }}
                onPointerDown={startDrag}
                onPointerMove={moveDrag}
                onPointerUp={endDrag}
                onPointerCancel={endDrag}
                aria-expanded={controlsOpen}
                aria-label={controlsOpen ? "Hide device controls" : "Show device controls"}
                title="Device controls — drag to another corner"
                className={cn(
                  "touch-none rounded-full border bg-background/85 shadow-sm backdrop-blur transition-opacity",
                  dragOffset ? "cursor-grabbing" : "cursor-grab",
                  controlsOpen ? "opacity-100" : "opacity-60 hover:opacity-100",
                )}
              >
                <MoreHorizontal className="size-4" />
              </Button>
            </div>
          )}

          {/* The overlay's controls hide themselves, so a recording started there
            would otherwise be running with nothing on screen to say so — and
            the popout is exactly where a window gets left alone. This sits
            outside the collapsing bar and is always visible while recording,
            and stops it, so nobody has to expand the bar to get their file. */}
          {controls === "overlay" && recording && (
            <button
              type="button"
              onClick={() => void finishRecording()}
              title="Stop recording"
              className={cn(
                "absolute flex h-7 items-center gap-1.5 rounded-full border border-destructive/40",
                "bg-background/85 px-2.5 font-medium text-destructive text-xs shadow-sm backdrop-blur",
                // The opposite side to the handle, so it never sits under the
                // controls it is not part of.
                corner.startsWith("t") ? "bottom-3" : "top-3",
                corner.endsWith("l") ? "right-3" : "left-3",
              )}
            >
              <span className="relative flex size-2">
                <span className="absolute inline-flex size-full animate-ping rounded-full bg-destructive opacity-75" />
                <span className="relative inline-flex size-2 rounded-full bg-destructive" />
              </span>
              <span className="tabular-nums">{formatElapsed(elapsed ?? 0)}</span>
            </button>
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
      </div>

      <input
        ref={installInput}
        type="file"
        accept=".apk,.ipa"
        className="hidden"
        onChange={(event) => {
          const file = event.target.files?.[0];
          // Cleared before the upload, so installing the same file twice in a
          // row still fires a change event the second time.
          event.target.value = "";
          if (file) void upload(file);
        }}
      />

      <DeviceFilesDialog
        deviceId={deviceId}
        platform={platform}
        open={filesOpen}
        onOpenChange={setFilesOpen}
      />
    </div>
  );
}

/** `m:ss`, which is all a two-minute cap ever needs. */
function formatElapsed(ms: number): string {
  const total = Math.floor(ms / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

type ConsoleAction = {
  key: string;
  label: string;
  icon: LucideIcon;
  run: () => void;
  danger?: boolean;
};

type ConsoleSection = { key: string; label: string; actions: ConsoleAction[] };

/**
 * Icon-only in both renderings, so the label lives in the tooltip and the
 * accessible name. That is why every action's `label` is a phrase rather than
 * a word — "Copy from device" says which direction, where a bare "Copy" beside
 * an unlabelled icon does not.
 */
function ActionButton({
  action,
  disabled,
  labelled = false,
}: {
  action: ConsoleAction;
  disabled: boolean;
  /** The rail spells the action out; the popout overlay has no room to. */
  labelled?: boolean;
}) {
  return (
    <Button
      variant="ghost"
      size={labelled ? "sm" : "icon"}
      disabled={disabled}
      title={action.label}
      // Redundant where the label is on screen, and a duplicate announcement.
      aria-label={labelled ? undefined : action.label}
      onClick={action.run}
      className={cn(labelled && "w-full justify-start", action.danger && "text-destructive")}
    >
      <action.icon className="size-4" />
      {labelled && <span className="truncate">{action.label}</span>}
    </Button>
  );
}
