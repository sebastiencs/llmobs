import { Link } from "@tanstack/react-router";
import { OrgSwitcher } from "@/components/org-switcher";
import { ThemeToggle } from "@/components/theme-toggle";
import { UserMenu } from "@/components/user-menu";
import { PRODUCT_NAME } from "@/product";

export function AppHeader() {
  return (
    <header className="flex h-14 items-center gap-3 border-b px-4">
      <Link to="/" className="font-medium">
        {PRODUCT_NAME}
      </Link>
      <OrgSwitcher />
      <div className="flex-1" />
      <ThemeToggle />
      <UserMenu />
    </header>
  );
}
