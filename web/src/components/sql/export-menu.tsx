import { Download } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  EXPORT_FORMATS,
  EXPORT_ORDER,
  EXPORT_ROLE_REASON,
  type ExportFormat,
} from "@/lib/sql/export";

export function ExportMenu({
  canExport,
  disabled,
  onExport,
}: {
  /** Role member or above (D-H-5). */
  canExport: boolean;
  /** No query, or a run or an export in flight. */
  disabled: boolean;
  onExport: (format: ExportFormat) => void;
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button type="button" variant="outline" size="xs">
          <Download />
          Export
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        {EXPORT_ORDER.map((format) => (
          <DropdownMenuItem
            key={format}
            disabled={disabled || !canExport}
            onSelect={() => onExport(format)}
          >
            <span className="flex flex-col">
              {EXPORT_FORMATS[format].label}
              {canExport ? null : (
                <span className="text-muted-foreground text-xs">{EXPORT_ROLE_REASON}</span>
              )}
            </span>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
