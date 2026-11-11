import { cn } from "cn";
import type { Schema } from "@/api/types";
import type { DataTableColumn } from "@/components/data-table/data-table";
import { TimeText } from "@/components/time-text";
import { formatCost, formatTokens } from "@/lib/format";
import { USER_COLUMN_IDS, type UserColumnId } from "@/lib/search/users";

const LABELS: Record<UserColumnId, string> = {
  user: "User",
  traces: "Traces",
  sessions: "Sessions",
  tokens: "Tokens",
  cost: "Cost",
  errors: "Errors",
  first_seen: "First seen",
  last_seen: "Last seen",
};

export function userColumnSpecs(): { id: string; label: string; defaultVisible: true }[] {
  return USER_COLUMN_IDS.map((id) => ({ id, label: LABELS[id], defaultVisible: true }));
}

export function orderedUserColumns(
  visible: readonly string[],
): DataTableColumn<Schema<"EndUserSummary">>[] {
  const byId = new Map(userColumnDefs().map((column) => [column.id, column]));
  return visible.flatMap((id) => {
    const column = byId.get(id);
    return column ? [column] : [];
  });
}

function userColumnDefs(): DataTableColumn<Schema<"EndUserSummary">>[] {
  return [
    {
      id: "user",
      size: 160,
      header: "User",
      enableSorting: false,
      meta: { mono: true },
      cell: ({ row }) => row.original.user_id,
    },
    {
      id: "traces",
      accessorFn: (row) => row.trace_count,
      size: 72,
      header: "Traces",
      enableSorting: true,
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{row.original.trace_count}</span>,
    },
    {
      id: "sessions",
      accessorFn: (row) => row.session_count,
      size: 88,
      header: "Sessions",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{row.original.session_count}</span>,
    },
    {
      id: "tokens",
      accessorFn: (row) => row.total_tokens,
      size: 80,
      header: "Tokens",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{formatTokens(row.original.total_tokens)}</span>,
    },
    {
      id: "cost",
      accessorFn: (row) => row.cost_total,
      size: 88,
      header: "Cost",
      enableSorting: true,
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{formatCost(row.original.cost_total)}</span>,
    },
    {
      id: "errors",
      accessorFn: (row) => row.error_count,
      size: 72,
      header: "Errors",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span className={cn("num", row.original.error_count > 0 && "text-destructive")}>
          {row.original.error_count}
        </span>
      ),
    },
    {
      id: "first_seen",
      accessorFn: (row) => row.first_seen,
      size: 168,
      header: "First seen",
      enableSorting: false,
      cell: ({ row }) => <TimeText value={row.original.first_seen} style="datetime" />,
    },
    {
      id: "last_seen",
      accessorFn: (row) => row.last_seen,
      size: 168,
      header: "Last seen",
      enableSorting: true,
      cell: ({ row }) => <TimeText value={row.original.last_seen} style="datetime" />,
    },
  ];
}
