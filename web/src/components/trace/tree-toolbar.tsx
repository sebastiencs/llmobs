import { ChevronDown, ChevronUp } from "lucide-react";
import type { KeyboardEvent } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function TreeToolbar({
  query,
  onQuery,
  matchLabel,
  onPrevious,
  onNext,
  errorsOnly,
  onErrorsOnly,
  onExpandAll,
  onCollapseDepth,
  onFit,
  zoomLabel,
}: {
  query: string;
  onQuery: (value: string) => void;
  matchLabel: string;
  onPrevious: () => void;
  onNext: () => void;
  errorsOnly: boolean;
  onErrorsOnly: (on: boolean) => void;
  onExpandAll: () => void;
  onCollapseDepth: () => void;
  onFit: () => void;
  zoomLabel: string;
}) {
  function onSearchKey(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key !== "Enter") {
      return;
    }
    event.preventDefault();
    if (event.shiftKey) {
      onPrevious();
    } else {
      onNext();
    }
  }

  return (
    <div className="flex h-8 shrink-0 items-center gap-1 border-b px-2">
      <Input
        value={query}
        type="search"
        aria-label="Search spans"
        placeholder="Search"
        className="h-6 min-w-0 flex-1 text-xs"
        onChange={(event) => onQuery(event.target.value)}
        onKeyDown={onSearchKey}
      />
      <span className="num shrink-0 text-[11px] text-muted-foreground">{matchLabel}</span>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        aria-label="Previous match"
        onClick={onPrevious}
      >
        <ChevronUp />
      </Button>
      <Button type="button" variant="ghost" size="icon-xs" aria-label="Next match" onClick={onNext}>
        <ChevronDown />
      </Button>
      <Button
        type="button"
        variant={errorsOnly ? "secondary" : "outline"}
        size="xs"
        aria-pressed={errorsOnly}
        onClick={() => onErrorsOnly(!errorsOnly)}
      >
        Errors only
      </Button>
      <Button type="button" variant="ghost" size="xs" onClick={onExpandAll}>
        Expand all
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="xs"
        aria-label="Collapse to depth 1"
        onClick={onCollapseDepth}
      >
        Collapse
      </Button>
      <span className="num shrink-0 text-[11px] text-muted-foreground">{zoomLabel}</span>
      <Button type="button" variant="outline" size="xs" onClick={onFit}>
        Fit
      </Button>
    </div>
  );
}
