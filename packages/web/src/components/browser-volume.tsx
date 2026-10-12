import { Volume1, Volume2, VolumeX } from "lucide-react";
import { useId } from "react";

export function BrowserVolume({
  volume,
  available,
  warning,
  onChange,
}: {
  volume: number;
  available: boolean;
  warning: string | null;
  onChange: (volume: number) => void;
}) {
  const statusId = useId();
  const Icon = volume === 0 ? VolumeX : volume < 50 ? Volume1 : Volume2;
  const control = (
    <div className="flex min-w-0 items-center gap-2">
      <Icon className="size-4 shrink-0 text-muted-foreground" />
      <input
        type="range"
        min={0}
        max={100}
        step={5}
        value={volume}
        disabled={!available}
        aria-describedby={!available ? statusId : undefined}
        aria-label="Browser audio volume. Does not change device volume."
        title="Browser audio only — does not change device volume"
        className="h-1.5 min-w-0 flex-1 cursor-pointer accent-primary disabled:cursor-not-allowed"
        onChange={(event) => onChange(Number(event.currentTarget.value))}
        onKeyDown={() => {
          if (available) onChange(volume);
        }}
      />
    </div>
  );

  return (
    <div className="flex flex-col gap-2 px-2">
      <div className="flex items-baseline justify-between gap-2">
        <span className="font-medium text-[10px] text-muted-foreground uppercase tracking-wide">
          Browser audio
        </span>
        <span className="text-[10px] text-muted-foreground tabular-nums">{volume}%</span>
      </div>
      {control}
      {!available && (
        <span
          id={statusId}
          role="status"
          title={warning ?? undefined}
          className="text-[10px] text-muted-foreground"
        >
          {warning ? "Audio unavailable" : "Connecting audio…"}
          {warning && <span className="sr-only">. {warning}</span>}
        </span>
      )}
    </div>
  );
}
