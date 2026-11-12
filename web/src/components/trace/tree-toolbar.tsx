import { ChevronDown, ChevronUp } from "lucide-react";
import type { KeyboardEvent } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { TraceMode } from "@/lib/search/traces";

type TreeControls = {
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
};

export function TreeToolbar({
  mode,
  onMode,
  ...tree
}: {
  mode: TraceMode;
  onMode: (mode: TraceMode) => void;
} & Partial<TreeControls>) {
  const controls = treeControls(tree);
  return (
    <div className="flex h-8 shrink-0 items-center gap-1 border-b px-2">
      <ModeToggle mode={mode} onMode={onMode} />
      {controls ? <TreeControls controls={controls} /> : null}
    </div>
  );
}

function ModeToggle({ mode, onMode }: { mode: TraceMode; onMode: (mode: TraceMode) => void }) {
  return (
    <div role="radiogroup" aria-label="Trace view" className="flex shrink-0">
      <Button
        type="button"
        role="radio"
        size="xs"
        variant={mode === "tree" ? "secondary" : "ghost"}
        aria-checked={mode === "tree"}
        onClick={() => onMode("tree")}
      >
        Tree
      </Button>
      <Button
        type="button"
        role="radio"
        size="xs"
        variant={mode === "transcript" ? "secondary" : "ghost"}
        aria-checked={mode === "transcript"}
        onClick={() => onMode("transcript")}
      >
        Transcript
      </Button>
    </div>
  );
}

function TreeControls({ controls }: { controls: TreeControls }) {
  function onSearchKey(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key !== "Enter") return;
    event.preventDefault();
    if (event.shiftKey) controls.onPrevious();
    else controls.onNext();
  }

  return (
    <>
      <Input
        value={controls.query}
        type="search"
        aria-label="Search spans"
        placeholder="Search"
        className="h-6 min-w-0 flex-1 text-xs"
        onChange={(event) => controls.onQuery(event.target.value)}
        onKeyDown={onSearchKey}
      />
      <span className="num shrink-0 text-[11px] text-muted-foreground">{controls.matchLabel}</span>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        aria-label="Previous match"
        onClick={controls.onPrevious}
      >
        <ChevronUp />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        aria-label="Next match"
        onClick={controls.onNext}
      >
        <ChevronDown />
      </Button>
      <Button
        type="button"
        variant={controls.errorsOnly ? "secondary" : "outline"}
        size="xs"
        aria-pressed={controls.errorsOnly}
        onClick={() => controls.onErrorsOnly(!controls.errorsOnly)}
      >
        Errors only
      </Button>
      <Button type="button" variant="ghost" size="xs" onClick={controls.onExpandAll}>
        Expand all
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="xs"
        aria-label="Collapse to depth 1"
        onClick={controls.onCollapseDepth}
      >
        Collapse
      </Button>
      <span className="num shrink-0 text-[11px] text-muted-foreground">{controls.zoomLabel}</span>
      <Button type="button" variant="outline" size="xs" onClick={controls.onFit}>
        Fit
      </Button>
    </>
  );
}

function treeControls(props: Partial<TreeControls>): TreeControls | undefined {
  if (
    props.query === undefined ||
    !props.onQuery ||
    props.matchLabel === undefined ||
    !props.onPrevious ||
    !props.onNext ||
    props.errorsOnly === undefined ||
    !props.onErrorsOnly ||
    !props.onExpandAll ||
    !props.onCollapseDepth ||
    !props.onFit ||
    props.zoomLabel === undefined
  ) {
    return undefined;
  }
  return {
    query: props.query,
    onQuery: props.onQuery,
    matchLabel: props.matchLabel,
    onPrevious: props.onPrevious,
    onNext: props.onNext,
    errorsOnly: props.errorsOnly,
    onErrorsOnly: props.onErrorsOnly,
    onExpandAll: props.onExpandAll,
    onCollapseDepth: props.onCollapseDepth,
    onFit: props.onFit,
    zoomLabel: props.zoomLabel,
  };
}
