import { Link, useMatchRoute } from "@tanstack/react-router";
import { type ReactNode, useState } from "react";
import { projectNav } from "@/components/shell/nav";
import { useProject } from "@/components/shell/project-context";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarGroupContent,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { TooltipProvider } from "@/components/ui/tooltip";
import { STORAGE_PREFIX } from "@/product";

const SIDEBAR_KEY = `${STORAGE_PREFIX}sidebar`;

function readSidebarOpen(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_KEY) !== "closed";
  } catch {
    return true;
  }
}

function writeSidebarOpen(open: boolean): void {
  try {
    localStorage.setItem(SIDEBAR_KEY, open ? "open" : "closed");
  } catch {
    // Private mode can throw. The choice still applies for this page.
  }
}

export function ProjectSidebar({ children }: { children: ReactNode }) {
  const { org, project } = useProject();
  const matchRoute = useMatchRoute();
  const [open, setOpen] = useState(readSidebarOpen);

  return (
    <TooltipProvider>
      <SidebarProvider
        open={open}
        onOpenChange={(next) => {
          setOpen(next);
          writeSidebarOpen(next);
        }}
        className="h-[calc(100dvh-40px)] overflow-hidden"
      >
        <Sidebar collapsible="icon">
          <SidebarHeader>
            <SidebarTrigger />
          </SidebarHeader>
          <SidebarContent>
            <SidebarGroup>
              <SidebarGroupContent>
                <SidebarMenu>
                  {projectNav.map((item) => (
                    <SidebarMenuItem key={item.to}>
                      <SidebarMenuButton
                        asChild
                        isActive={Boolean(
                          matchRoute({ to: item.to, params: { org, project }, fuzzy: true }),
                        )}
                        tooltip={item.label}
                      >
                        <Link
                          to={item.to}
                          params={{ org, project }}
                          state={(current) => ({ ...current, applyDefaultView: true })}
                        >
                          <item.icon />
                          <span>{item.label}</span>
                        </Link>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  ))}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          </SidebarContent>
        </Sidebar>
        <SidebarInset className="min-h-0 overflow-hidden">{children}</SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  );
}
