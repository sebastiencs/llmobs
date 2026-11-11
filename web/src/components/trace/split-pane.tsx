import { type ReactNode, useEffect, useState } from "react";
import { useGroupRef } from "react-resizable-panels";
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from "@/components/ui/resizable";
import { STORAGE_PREFIX } from "@/product";

type SplitName = "traces" | "trace";

const PANES = {
  traces: { start: "traces-list", end: "traces-detail", startSize: 40 },
  trace: { start: "trace-tree", end: "trace-detail", startSize: 55 },
} as const;

export function SplitPane({
  name,
  open = true,
  start,
  end,
}: {
  name: SplitName;
  open?: boolean;
  start: ReactNode;
  end: ReactNode;
}) {
  const panes = PANES[name];
  const groupRef = useGroupRef();
  const [initial] = useState(() =>
    open ? (readSplit(name) ?? fallback(name)) : closedLayout(name),
  );

  useEffect(() => {
    const handle = groupRef.current;
    if (!handle) {
      return;
    }
    // Both panels stay mounted, so opening the detail does not remount the list.
    handle.setLayout(open ? (readSplit(name) ?? fallback(name)) : closedLayout(name));
  }, [groupRef, name, open]);

  return (
    <ResizablePanelGroup
      id={`split-${name}`}
      groupRef={groupRef}
      orientation="horizontal"
      disabled={!open}
      className="h-full min-h-0"
      defaultLayout={initial}
      onLayoutChanged={(_layout, meta) => {
        if (!open || !meta.isUserInteraction || !meta.requestedLayout) {
          return;
        }
        writeSplit(name, meta.requestedLayout);
      }}
    >
      <ResizablePanel
        id={panes.start}
        minSize={open ? "15%" : "0%"}
        className="relative"
        style={{ overflow: "hidden" }}
      >
        <div className="absolute inset-0 flex min-h-0 flex-col">{start}</div>
      </ResizablePanel>
      {open ? <ResizableHandle /> : null}
      <ResizablePanel
        id={panes.end}
        minSize={open ? "15%" : "0%"}
        className="relative"
        style={{ overflow: "hidden" }}
      >
        <div className="absolute inset-0 flex min-h-0 flex-col">{open ? end : null}</div>
      </ResizablePanel>
    </ResizablePanelGroup>
  );
}

function closedLayout(name: SplitName): Record<string, number> {
  const panes = PANES[name];
  return { [panes.start]: 100, [panes.end]: 0 };
}

function fallback(name: SplitName): Record<string, number> {
  const panes = PANES[name];
  return { [panes.start]: panes.startSize, [panes.end]: 100 - panes.startSize };
}

function readSplit(name: SplitName): Record<string, number> | undefined {
  const panes = PANES[name];
  try {
    const raw = localStorage.getItem(`${STORAGE_PREFIX}split.${name}`);
    if (!raw) {
      return undefined;
    }
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) {
      return undefined;
    }
    const record = parsed as Record<string, unknown>;
    const start = record[panes.start];
    const end = record[panes.end];
    if (!isPercent(start) || !isPercent(end)) {
      return undefined;
    }
    return { [panes.start]: start, [panes.end]: end };
  } catch {
    return undefined;
  }
}

function writeSplit(name: SplitName, layout: Record<string, number>): void {
  try {
    localStorage.setItem(`${STORAGE_PREFIX}split.${name}`, JSON.stringify(layout));
  } catch {
    // Private mode and quota failures leave the in-memory ratio in place.
  }
}

function isPercent(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 && value < 100;
}
