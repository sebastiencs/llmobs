import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Toggle } from "@/components/ui/toggle";
import { type TimeZoneSetting, useTimeZone } from "@/lib/settings";
import {
  PRESETS,
  type Preset,
  parseTypedRange,
  presetLabel,
  type RangeState,
  rangeLabel,
  resolveRange,
} from "@/lib/time-range";

const TYPED_HINT = "Use 15m, 2h, 7d…";

export function TimeRangePicker({
  state,
  onChange,
}: {
  state: RangeState;
  onChange: (state: RangeState) => void;
}) {
  const zone = useTimeZone();
  const [open, setOpen] = useState(false);
  const [typed, setTyped] = useState("");
  const [typedError, setTypedError] = useState(false);
  const [custom, setCustom] = useState(() => customSeed(state, zone));
  const [customError, setCustomError] = useState(false);

  function apply(next: RangeState) {
    onChange(next);
    setOpen(false);
    setTyped("");
    setTypedError(false);
    setCustomError(false);
  }

  function applyTyped() {
    const parsed = parseTypedRange(typed);
    if (!parsed) {
      setTypedError(true);
      return;
    }
    if (typeof parsed === "string") {
      apply({ kind: "preset", preset: parsed });
      return;
    }
    const now = Date.now();
    apply({
      kind: "absolute",
      from: new Date(now - parsed.durationMs).toISOString(),
      to: new Date(now).toISOString(),
    });
  }

  function applyCustom() {
    const from = fromDateTimeLocal(custom.from, zone);
    const to = fromDateTimeLocal(custom.to, zone);
    if (!from || !to || Date.parse(from) >= Date.parse(to)) {
      setCustomError(true);
      return;
    }
    apply({ kind: "absolute", from, to });
  }

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) {
          setCustom(customSeed(state, zone));
          setCustomError(false);
          setTypedError(false);
        }
      }}
    >
      <PopoverTrigger asChild>
        <Button variant="outline" size="xs" className="font-normal">
          {rangeLabel(state, zone)}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-64 gap-3 p-3">
        <div className="flex flex-col">
          {PRESETS.map((preset) => (
            <PresetRow
              key={preset}
              preset={preset}
              pressed={state.kind === "preset" && state.preset === preset}
              onSelect={() => apply({ kind: "preset", preset })}
            />
          ))}
        </div>
        <form
          className="flex flex-col gap-1"
          onSubmit={(event) => {
            event.preventDefault();
            applyTyped();
          }}
        >
          <Label htmlFor="range-duration" className="text-xs font-normal text-muted-foreground">
            Duration
          </Label>
          <Input
            id="range-duration"
            value={typed}
            placeholder="2h"
            aria-invalid={typedError}
            className="h-7 text-xs"
            onChange={(event) => {
              setTyped(event.target.value);
              setTypedError(false);
            }}
          />
          {typedError ? <p className="text-xs text-destructive">{TYPED_HINT}</p> : null}
        </form>
        <form
          className="flex flex-col gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            applyCustom();
          }}
        >
          <div className="flex flex-col gap-1">
            <Label htmlFor="range-from" className="text-xs font-normal text-muted-foreground">
              From
            </Label>
            <Input
              id="range-from"
              type="datetime-local"
              value={custom.from}
              aria-invalid={customError}
              className="h-7 text-xs"
              onChange={(event) => {
                setCustom((current) => ({ ...current, from: event.target.value }));
                setCustomError(false);
              }}
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="range-to" className="text-xs font-normal text-muted-foreground">
              To
            </Label>
            <Input
              id="range-to"
              type="datetime-local"
              value={custom.to}
              aria-invalid={customError}
              className="h-7 text-xs"
              onChange={(event) => {
                setCustom((current) => ({ ...current, to: event.target.value }));
                setCustomError(false);
              }}
            />
          </div>
          {customError ? <p className="text-xs text-destructive">From must be before to.</p> : null}
          <Button type="submit" size="xs" variant="secondary">
            Apply
          </Button>
        </form>
      </PopoverContent>
    </Popover>
  );
}

function PresetRow({
  preset,
  pressed,
  onSelect,
}: {
  preset: Preset;
  pressed: boolean;
  onSelect: () => void;
}) {
  return (
    <Toggle
      pressed={pressed}
      size="sm"
      className="h-7 w-full justify-start px-2 text-xs font-normal"
      onPressedChange={onSelect}
    >
      {presetLabel(preset)}
    </Toggle>
  );
}

function customSeed(state: RangeState, zone: TimeZoneSetting): { from: string; to: string } {
  const resolved = state.kind === "absolute" ? state : resolveRange(state, Date.now());
  return {
    from: toDateTimeLocal(resolved.from, zone),
    to: toDateTimeLocal(resolved.to, zone),
  };
}

function toDateTimeLocal(rfc3339: string, zone: TimeZoneSetting): string {
  const date = new Date(rfc3339);
  if (Number.isNaN(date.getTime())) {
    return "";
  }
  const utc = zone === "utc";
  const year = utc ? date.getUTCFullYear() : date.getFullYear();
  const month = (utc ? date.getUTCMonth() : date.getMonth()) + 1;
  const day = utc ? date.getUTCDate() : date.getDate();
  const hours = utc ? date.getUTCHours() : date.getHours();
  const minutes = utc ? date.getUTCMinutes() : date.getMinutes();
  return `${year}-${pad(month)}-${pad(day)}T${pad(hours)}:${pad(minutes)}`;
}

function fromDateTimeLocal(value: string, zone: TimeZoneSetting): string | undefined {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(value);
  if (!match) {
    return undefined;
  }
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const hours = Number(match[4]);
  const minutes = Number(match[5]);
  const epoch =
    zone === "utc"
      ? Date.UTC(year, month - 1, day, hours, minutes)
      : new Date(year, month - 1, day, hours, minutes).getTime();
  if (Number.isNaN(epoch)) {
    return undefined;
  }
  return new Date(epoch).toISOString();
}

function pad(value: number): string {
  return String(value).padStart(2, "0");
}
