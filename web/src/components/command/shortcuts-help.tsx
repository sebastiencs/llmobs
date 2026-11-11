import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { SHORTCUTS } from "@/lib/shortcuts";

export function ShortcutsHelp({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Keyboard shortcuts</DialogTitle>
          <DialogDescription>These shortcuts are ignored while you are typing.</DialogDescription>
        </DialogHeader>
        <table className="w-full text-sm">
          <thead>
            <tr className="text-left text-xs text-muted-foreground">
              <th className="pr-3 pb-2 font-medium">Keys</th>
              <th className="pr-3 pb-2 font-medium">Action</th>
              <th className="pb-2 font-medium">Where</th>
            </tr>
          </thead>
          <tbody>
            {SHORTCUTS.map((row) => (
              <tr key={row.keys} className="border-t">
                <td className="py-1.5 pr-3 font-mono text-xs whitespace-nowrap">{row.keys}</td>
                <td className="py-1.5 pr-3">{row.description}</td>
                <td className="py-1.5 whitespace-nowrap text-muted-foreground">{row.scope}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </DialogContent>
    </Dialog>
  );
}
