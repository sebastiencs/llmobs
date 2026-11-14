import { Clock } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { clearHistory, type HistoryEntry, historyLabel, readHistory } from "@/lib/sql/history";

export function HistoryMenu({
  projectRef,
  onPick,
}: {
  projectRef: string;
  onPick: (entry: HistoryEntry) => void;
}) {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  return (
    <DropdownMenu
      onOpenChange={(open) => {
        if (open) {
          setEntries(readHistory(projectRef));
        }
      }}
    >
      <DropdownMenuTrigger asChild>
        <Button type="button" variant="outline" size="xs">
          <Clock />
          History
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-72">
        {entries.length === 0 ? <DropdownMenuItem disabled>No runs yet</DropdownMenuItem> : null}
        {entries.map((entry) => (
          <DropdownMenuItem
            key={`${entry.ranAt}|${JSON.stringify(entry.params)}|${entry.query}`}
            title={entry.query}
            onSelect={() => onPick(entry)}
          >
            <span className="min-w-0 flex-1 truncate font-mono">{historyLabel(entry.query)}</span>
            <span className="text-muted-foreground">
              {entry.rows === undefined
                ? "failed"
                : `${entry.rows} ${entry.rows === 1 ? "row" : "rows"}`}
            </span>
          </DropdownMenuItem>
        ))}
        {entries.length > 0 ? (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              onSelect={() => {
                clearHistory(projectRef);
                setEntries([]);
              }}
            >
              Clear history
            </DropdownMenuItem>
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
