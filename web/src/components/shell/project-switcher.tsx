import { useMatch, useNavigate, useParams } from "@tanstack/react-router";
import { Loader2 } from "lucide-react";
import { $api } from "@/api/client";
import { projectListInit } from "@/api/queries";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

export function ProjectSwitcher() {
  const { org } = useParams({ from: "/$org" });
  const projectMatch = useMatch({ from: "/$org/$project", shouldThrow: false });
  const project = projectMatch?.params.project;
  const navigate = useNavigate();
  const projects = $api.useInfiniteQuery(
    "get",
    "/api/v1/orgs/{org}/projects",
    projectListInit(org),
    {
      pageParamName: "cursor",
      initialPageParam: null,
      getNextPageParam: (page) => page.next_cursor ?? undefined,
    },
  );
  const rows = projects.data?.pages.flatMap((page) => page.data) ?? [];
  const current = rows.find((item) => item.slug === project);
  const label = current?.name ?? project ?? "Project";

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" aria-label={label}>
          {label}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start">
        <DropdownMenuRadioGroup
          value={project ?? ""}
          onValueChange={(slug) => {
            void navigate({ to: "/$org/$project/traces", params: { org, project: slug } });
          }}
        >
          {rows.map((item) => (
            <DropdownMenuRadioItem key={item.id} value={item.slug}>
              {item.name}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
        {projects.hasNextPage ? (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              disabled={projects.isFetchingNextPage}
              onSelect={(event) => {
                event.preventDefault();
                void projects.fetchNextPage();
              }}
            >
              {projects.isFetchingNextPage ? <Loader2 className="animate-spin" /> : null}
              Load more
            </DropdownMenuItem>
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
