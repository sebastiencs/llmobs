import { createFileRoute } from "@tanstack/react-router";
import { useProject } from "@/components/shell/project-context";
import { SqlConsole } from "@/components/sql/sql-console";
import { parseSqlSearch } from "@/lib/search/sql";

export const Route = createFileRoute("/$org/$project/sql/")({
  validateSearch: (search) => parseSqlSearch(search),
  component: SqlPage,
});

function SqlPage() {
  const { org, project, ref } = useProject();
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  return (
    <SqlConsole
      key={ref}
      search={search}
      onSearch={(update) => {
        void navigate({
          to: "/$org/$project/sql",
          params: { org, project },
          search: (previous) => update(previous),
        });
      }}
    />
  );
}
