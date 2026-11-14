import { Rows3 } from "lucide-react";
import { memo, useMemo } from "react";
import type { Schema } from "@/api/types";
import { DataTable, type DataTableColumn } from "@/components/data-table/data-table";
import { EmptyState } from "@/components/empty-state";
import { isNumericType } from "@/lib/sql/types";

export const RESULT_TABLE_ID = "sql-results";

export type SqlRow = { index: number; values: Record<string, unknown> };

/** Display text of one JSON cell. `null` and a missing key read `NULL`; objects are JSON. */
export function formatSqlCell(value: unknown): string {
  if (value === null || value === undefined) {
    return "NULL";
  }
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return JSON.stringify(value);
}

export function sqlColumns(result: Schema<"SqlQueryResult">): DataTableColumn<SqlRow>[] {
  return result.columns.map((column, index) => ({
    // Column names may hold spaces; DataTable builds DOM ids from the column id.
    id: `c${index}`,
    size: 160,
    header: column.name,
    enableSorting: false,
    meta: { mono: true, align: isNumericType(column.type) ? "right" : "left" },
    cell: ({ row }) => {
      const values = row.original.values;
      // Own keys only: a column named `__proto__` or `constructor` must not read the prototype.
      const value = Object.hasOwn(values, column.name) ? values[column.name] : undefined;
      const text = formatSqlCell(value);
      const isNull = value === null || value === undefined;
      return (
        <span title={text} className={isNull ? "text-muted-foreground" : undefined}>
          {text}
        </span>
      );
    },
  }));
}

function sqlRows(result: Schema<"SqlQueryResult">): SqlRow[] {
  return result.rows.map((values, index) => ({
    index,
    values: values as Record<string, unknown>,
  }));
}

export const SqlResultGrid = memo(function SqlResultGrid({
  result,
}: {
  result: Schema<"SqlQueryResult">;
}) {
  const columns = useMemo(() => sqlColumns(result), [result]);
  const rows = useMemo(() => sqlRows(result), [result]);
  return (
    <DataTable
      tableId={RESULT_TABLE_ID}
      ariaLabel="Query results"
      columns={columns}
      rows={rows}
      getRowId={(row) => String(row.index)}
      onActivate={() => {}}
      hasNextPage={false}
      isFetchingNextPage={false}
      fetchNextPage={() => {}}
      emptyState={<EmptyState icon={Rows3} title="No rows" />}
    />
  );
});
