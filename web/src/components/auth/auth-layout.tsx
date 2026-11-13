import type { ReactNode } from "react";
import { ThemeToggle } from "@/components/theme-toggle";
import { PRODUCT_NAME } from "@/product";

export function AuthLayout({
  title,
  description,
  children,
  footer,
  width = "narrow",
}: {
  title: string;
  description?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  width?: "narrow" | "wide";
}) {
  return (
    <div className="flex min-h-dvh flex-col">
      <header className="flex h-[40px] shrink-0 items-center justify-between px-3">
        <span className="font-medium">{PRODUCT_NAME}</span>
        <ThemeToggle />
      </header>
      <main
        className={`mx-auto flex w-full flex-1 flex-col justify-center gap-6 px-4 py-10 ${
          width === "wide" ? "max-w-2xl" : "max-w-sm"
        }`}
      >
        <div className="flex flex-col gap-1">
          <h1 className="text-xl font-medium">{title}</h1>
          {description ? <div className="text-sm text-muted-foreground">{description}</div> : null}
        </div>
        {children}
        {footer ? <div className="flex flex-col gap-1 text-sm">{footer}</div> : null}
      </main>
    </div>
  );
}
