import { Palette } from "lucide-react";
import {
  DropdownMenuPortal,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
} from "@/components/ui/dropdown-menu";
import { ACCENTS, type Accent, useAccent } from "@/lib/accent";

/** Picks the colour your own reservations are highlighted in. */
export function AccentToggle() {
  const { accent, setAccent } = useAccent();

  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        <Palette className="size-4" />
        Highlight
      </DropdownMenuSubTrigger>
      <DropdownMenuPortal>
        <DropdownMenuSubContent>
          <DropdownMenuRadioGroup value={accent} onValueChange={(v) => setAccent(v as Accent)}>
            {ACCENTS.map(({ value, label }) => (
              <DropdownMenuRadioItem key={value} value={value}>
                {/* The swatch is the label: the names only matter for a reader
                    who cannot see the colour they are choosing. */}
                <span data-accent={value} className="size-3 rounded-full bg-mine" aria-hidden />
                {label}
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuSubContent>
      </DropdownMenuPortal>
    </DropdownMenuSub>
  );
}
