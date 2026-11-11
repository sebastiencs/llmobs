// The plan requires a CSS grid with grid/row/gridcell roles so rows can be
// virtualized. The grid is the single tab stop; arrows move an internal focus
// and Enter activates the focused row.
// biome-ignore-all lint/a11y/useSemanticElements: virtualized CSS grid, not a table
// biome-ignore-all lint/a11y/useFocusableInteractive: one tab stop on the grid
// biome-ignore-all lint/a11y/useKeyWithClickEvents: Enter on the grid activates the row

import {
  type ColumnDef,
  columnSizingFeature,
  flexRender,
  type RowData,
  rowSortingFeature,
  tableFeatures,
  useTable,
} from "@tanstack/react-table";
import { useVirtualizer } from "@tanstack/react-virtual";
import { cn } from "cn";
import { ArrowDown, ArrowUp, Loader2 } from "lucide-react";
import { type KeyboardEvent, type ReactNode, useEffect, useRef, useState } from "react";
import {
  columnSizesAfterReset,
  columnSizesAfterResize,
  readColumnSizes,
  writeColumnSizes,
} from "@/components/data-table/column-size";
import { matchesBinding } from "@/lib/shortcuts";

const features = tableFeatures({
  columnSizingFeature,
  rowSortingFeature,
  columnMeta: {} as { align?: "left" | "right"; mono?: boolean },
});

export type DataTableColumn<Row extends RowData> = ColumnDef<typeof features, Row>;

const ROW_HEIGHT = 28;
const OVERSCAN = 12;
const FETCH_AHEAD = 20;

export interface DataTableProps<Row extends RowData> {
  tableId: string;
  ariaLabel: string;
  columns: DataTableColumn<Row>[];
  rows: Row[];
  getRowId: (row: Row) => string;
  selectedRowId?: string;
  onActivate: (row: Row) => void;
  onSelectionMove?: (row: Row) => void;
  hasNextPage: boolean;
  isFetchingNextPage: boolean;
  fetchNextPage: () => void;
  sorting?: { id: string; desc: boolean };
  onSortingChange?: (next: { id: string; desc: boolean } | null) => void;
  emptyState: ReactNode;
}

export function DataTable<Row extends RowData>({
  tableId,
  ariaLabel,
  columns,
  rows,
  getRowId,
  selectedRowId,
  onActivate,
  onSelectionMove,
  hasNextPage,
  isFetchingNextPage,
  fetchNextPage,
  sorting,
  onSortingChange,
  emptyState,
}: DataTableProps<Row>) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [columnSizing, setColumnSizing] = useState(() => readColumnSizes(tableId));
  const [focusedIndex, setFocusedIndex] = useState<number | null>(null);
  const stopDrag = useRef<(() => void) | null>(null);

  useEffect(() => () => stopDrag.current?.(), []);

  function commit(next: Record<string, number>) {
    setColumnSizing(next);
    writeColumnSizes(tableId, next);
  }

  const table = useTable({
    features,
    columns,
    data: rows,
    defaultColumn: { enableSorting: false },
    manualSorting: true,
    getRowId: (row) => getRowId(row),
    state: {
      columnSizing,
      sorting: sorting ? [{ id: sorting.id, desc: sorting.desc }] : [],
    },
    onColumnSizingChange: (updater) => {
      const next = typeof updater === "function" ? updater(columnSizing) : updater;
      commit(next);
    },
  });

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: OVERSCAN,
  });

  const virtualRows = virtualizer.getVirtualItems();
  const lastIndex = virtualRows.at(-1)?.index ?? -1;

  useEffect(() => {
    if (!hasNextPage || isFetchingNextPage || rows.length === 0) {
      return;
    }
    if (lastIndex >= rows.length - FETCH_AHEAD) {
      fetchNextPage();
    }
  }, [fetchNextPage, hasNextPage, isFetchingNextPage, lastIndex, rows.length]);

  useEffect(() => {
    if (focusedIndex !== null && focusedIndex >= rows.length) {
      setFocusedIndex(rows.length === 0 ? null : rows.length - 1);
    }
  }, [focusedIndex, rows.length]);

  function moveFocus(next: number) {
    if (rows.length === 0) {
      return;
    }
    const index = Math.min(rows.length - 1, Math.max(0, next));
    if (index === focusedIndex) {
      return;
    }
    setFocusedIndex(index);
    virtualizer.scrollToIndex(index, { align: "auto" });
    if (selectedRowId !== undefined) {
      const row = rows[index];
      if (row) {
        onSelectionMove?.(row);
      }
    }
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (rows.length === 0) {
      return;
    }
    const origin = focusedIndex ?? -1;
    if (matchesBinding(event, "row-next")) {
      event.preventDefault();
      moveFocus(origin + 1);
    } else if (matchesBinding(event, "row-prev")) {
      event.preventDefault();
      moveFocus(origin - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      moveFocus(0);
    } else if (event.key === "End") {
      event.preventDefault();
      moveFocus(rows.length - 1);
    } else if (matchesBinding(event, "open") && focusedIndex !== null) {
      event.preventDefault();
      const row = rows[focusedIndex];
      if (row) {
        onActivate(row);
      }
    }
  }

  function beginResize(event: React.PointerEvent<HTMLDivElement>, id: string, size: number) {
    event.preventDefault();
    event.stopPropagation();
    const startX = event.clientX;
    const move = (ev: PointerEvent) => {
      commit(columnSizesAfterResize(columnSizing, id, size + (ev.clientX - startX)));
    };
    const stop = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
      if (stopDrag.current === stop) {
        stopDrag.current = null;
      }
    };
    stopDrag.current?.();
    stopDrag.current = stop;
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
  }

  const headerGroup = table.getHeaderGroups()[0];
  const template = headerGroup
    ? headerGroup.headers.map((header) => `${header.getSize()}px`).join(" ")
    : undefined;
  const model = table.getRowModel().rows;

  return (
    <div
      role="grid"
      aria-label={ariaLabel}
      aria-rowcount={rows.length + 1 + (isFetchingNextPage ? 1 : 0)}
      aria-colcount={columns.length}
      tabIndex={0}
      className="flex min-h-0 flex-1 flex-col outline-none"
      onKeyDown={onKeyDown}
    >
      {headerGroup ? (
        <div
          role="row"
          aria-rowindex={1}
          className="grid h-row shrink-0 items-center border-b bg-background text-[11px] font-medium text-muted-foreground uppercase"
          style={{ gridTemplateColumns: template }}
        >
          {headerGroup.headers.map((header) => {
            const sorted = header.column.getIsSorted();
            const canSort = header.column.getCanSort();
            const label =
              typeof header.column.columnDef.header === "string"
                ? header.column.columnDef.header
                : "column";
            return (
              <div
                key={header.id}
                role="columnheader"
                aria-labelledby={`${tableId}-${header.id}-label`}
                aria-sort={
                  canSort
                    ? sorted === "asc"
                      ? "ascending"
                      : sorted === "desc"
                        ? "descending"
                        : "none"
                    : undefined
                }
                className={cn(
                  "relative flex h-full min-w-0 items-center px-2",
                  header.column.columnDef.meta?.align === "right" && "justify-end",
                )}
              >
                {canSort && onSortingChange ? (
                  <button
                    type="button"
                    id={`${tableId}-${header.id}-label`}
                    className="inline-flex min-w-0 items-center gap-1"
                    onClick={() => {
                      if (sorting?.id !== header.column.id) {
                        onSortingChange({ id: header.column.id, desc: true });
                        return;
                      }
                      if (sorting.desc) {
                        onSortingChange({ id: header.column.id, desc: false });
                        return;
                      }
                      onSortingChange(null);
                    }}
                  >
                    <span className="truncate">
                      {flexRender(header.column.columnDef.header, header.getContext())}
                    </span>
                    <SortArrow direction={sorted} />
                  </button>
                ) : (
                  <span id={`${tableId}-${header.id}-label`} className="min-w-0 truncate">
                    {flexRender(header.column.columnDef.header, header.getContext())}
                  </span>
                )}
                <div
                  role="separator"
                  aria-orientation="vertical"
                  aria-label={`Resize ${label}`}
                  className="absolute inset-y-0 right-0 w-1 cursor-col-resize"
                  onPointerDown={(event) => beginResize(event, header.column.id, header.getSize())}
                  onDoubleClick={(event) => {
                    event.preventDefault();
                    event.stopPropagation();
                    commit(columnSizesAfterReset(columnSizing, header.column.id));
                  }}
                />
              </div>
            );
          })}
        </div>
      ) : null}
      <div ref={scrollRef} data-scroll-container className="relative min-h-0 flex-1 overflow-auto">
        {rows.length === 0 ? (
          emptyState
        ) : (
          <div
            className="relative"
            style={{ height: virtualizer.getTotalSize() + (isFetchingNextPage ? ROW_HEIGHT : 0) }}
          >
            {virtualRows.map((item) => {
              const row = model[item.index];
              if (!row) {
                return null;
              }
              const selected = row.id === selectedRowId;
              const focused = focusedIndex === item.index;
              return (
                <div
                  key={row.id}
                  role="row"
                  aria-rowindex={item.index + 2}
                  aria-selected={selected}
                  data-focused={focused ? "" : undefined}
                  className={cn(
                    "absolute top-0 grid w-full items-center border-b",
                    selected
                      ? "bg-selection shadow-[inset_2px_0_0_var(--primary)]"
                      : "hover:bg-muted",
                    focused && !selected && "bg-muted",
                  )}
                  style={{
                    height: ROW_HEIGHT,
                    transform: `translateY(${item.start}px)`,
                    gridTemplateColumns: template,
                  }}
                  onClick={() => {
                    setFocusedIndex(item.index);
                    onActivate(row.original);
                  }}
                >
                  {row.getAllCells().map((cell) => (
                    <div
                      key={cell.id}
                      role="gridcell"
                      className={cn(
                        "min-w-0 truncate px-2",
                        cell.column.columnDef.meta?.align === "right" && "text-right",
                        cell.column.columnDef.meta?.mono && "font-mono",
                      )}
                    >
                      {flexRender(cell.column.columnDef.cell, cell.getContext())}
                    </div>
                  ))}
                </div>
              );
            })}
            {isFetchingNextPage ? (
              <div
                role="row"
                aria-rowindex={rows.length + 2}
                className="absolute right-0 left-0 flex items-center px-2 text-muted-foreground"
                style={{ top: virtualizer.getTotalSize(), height: ROW_HEIGHT }}
              >
                <Loader2 className="size-3 animate-spin" />
                <span className="sr-only">Loading more</span>
              </div>
            ) : null}
          </div>
        )}
      </div>
    </div>
  );
}

function SortArrow({ direction }: { direction: false | "asc" | "desc" }) {
  if (direction === "asc") {
    return <ArrowUp aria-hidden className="size-3 shrink-0" />;
  }
  if (direction === "desc") {
    return <ArrowDown aria-hidden className="size-3 shrink-0" />;
  }
  return null;
}
