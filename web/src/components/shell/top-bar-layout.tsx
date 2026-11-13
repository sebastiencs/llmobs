import type { ReactNode } from "react";
import { TopBar } from "@/components/shell/top-bar";

/** Full-height page with the top bar. Children fill the rest and scroll themselves. */
export function TopBarLayout({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-dvh flex-col overflow-hidden">
      <TopBar />
      <div className="flex min-h-0 flex-1 flex-col">{children}</div>
    </div>
  );
}
