import { Link, useMatch } from "@tanstack/react-router";
import { CommandPalette } from "@/components/command/palette";
import { OrgSwitcher } from "@/components/org-switcher";
import { ProjectSwitcher } from "@/components/shell/project-switcher";
import { ThemeToggle } from "@/components/theme-toggle";
import { UserMenu } from "@/components/user-menu";
import { PRODUCT_NAME } from "@/product";

export function TopBar() {
  const inOrg = useMatch({ from: "/$org", shouldThrow: false }) !== undefined;
  return (
    <header className="flex h-[40px] shrink-0 items-center gap-2 border-b px-3">
      <Link
        to="/"
        className="rounded-sm font-medium outline-none focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2 focus-visible:outline-solid"
      >
        {PRODUCT_NAME}
      </Link>
      {inOrg ? (
        <>
          <OrgSwitcher />
          <span aria-hidden="true" className="text-muted-foreground">
            /
          </span>
          <ProjectSwitcher />
        </>
      ) : null}
      <div className="flex-1" />
      {inOrg ? <CommandPalette /> : null}
      <ThemeToggle />
      <UserMenu />
    </header>
  );
}
