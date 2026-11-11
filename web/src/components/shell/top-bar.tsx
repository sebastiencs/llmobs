import { Link } from "@tanstack/react-router";
import { OrgSwitcher } from "@/components/org-switcher";
import { ProjectSwitcher } from "@/components/shell/project-switcher";
import { ThemeToggle } from "@/components/theme-toggle";
import { UserMenu } from "@/components/user-menu";
import { PRODUCT_NAME } from "@/product";

export function TopBar() {
  return (
    <header className="flex h-[40px] shrink-0 items-center gap-2 border-b px-3">
      <Link to="/" className="font-medium">
        {PRODUCT_NAME}
      </Link>
      <OrgSwitcher />
      <span aria-hidden="true" className="text-muted-foreground">
        /
      </span>
      <ProjectSwitcher />
      <div className="flex-1" />
      <ThemeToggle />
      <UserMenu />
    </header>
  );
}
