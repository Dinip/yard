import { PanelLeftClose, PanelLeftOpen, PanelRightClose, PanelRightOpen } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/**
 * A collapsible column beside the device screen.
 *
 * Both of the device page's flanks are this: the control rail on the left and
 * the Details panel on the right. They collapse for the same reason — a device
 * is used in portrait, so the screen wants every pixel that is not doing
 * something — and they behave identically, which is why this is one component
 * rather than two that drift.
 *
 * Collapsed, the panel is exactly the toggle plus its padding. That width is
 * deliberate and not a round number: `w-10` with `p-2` leaves 24px for a 36px
 * icon button, and the overflow gets clipped off-centre.
 */
export function SidePanel({
  side,
  open,
  onToggle,
  title,
  width,
  className,
  children,
}: {
  side: "left" | "right";
  open: boolean;
  onToggle: () => void;
  /** Names the panel in its header and in the toggle's accessible name. */
  title: string;
  /** Tailwind width class applied only while open. */
  width: string;
  className?: string;
  children: ReactNode;
}) {
  const Icon = open
    ? side === "left"
      ? PanelLeftClose
      : PanelRightClose
    : side === "left"
      ? PanelLeftOpen
      : PanelRightOpen;
  const label = `${open ? "Hide" : "Show"} ${title.toLowerCase()}`;

  return (
    <div
      className={cn(
        "flex min-h-0 shrink-0 flex-col overflow-hidden",
        open ? width : "w-11",
        className,
      )}
    >
      <div
        className={cn(
          "flex shrink-0 items-center gap-2",
          open ? "border-b p-2" : "justify-center p-1",
        )}
      >
        <Button
          variant="ghost"
          size="icon"
          onClick={onToggle}
          aria-expanded={open}
          aria-label={label}
          title={label}
          // The toggle sits on the panel's inner edge in both directions, so it
          // stays next to the screen it is making room for.
          className={cn(side === "left" && "order-last")}
        >
          <Icon className="size-4" />
        </Button>
        {/* On the left panel the title sits against the outer edge while the
            toggle sits against the inner one, and the two looked unbalanced:
            the button is 36px around a 16px icon, so its glyph is already 10px
            inside the header's padding and the raw text was not. `pl-2.5` gives
            the text the same 18px inset. The right panel needs none — there the
            button is the outer element and the title just follows the gap. */}
        {open && (
          <span
            className={cn(
              "min-w-0 flex-1 truncate font-medium text-sm",
              side === "left" && "pl-2.5",
            )}
          >
            {title}
          </span>
        )}
      </div>
      {open && <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>}
    </div>
  );
}
