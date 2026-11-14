import { BookOpen } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { SQL_EXAMPLES, type SqlExample } from "@/lib/sql/examples";

export function ExamplesMenu({ onPick }: { onPick: (example: SqlExample) => void }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button type="button" variant="outline" size="xs">
          <BookOpen />
          Examples
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-56">
        {SQL_EXAMPLES.map((example) => (
          <DropdownMenuItem key={example.name} onSelect={() => onPick(example)}>
            {example.name}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
