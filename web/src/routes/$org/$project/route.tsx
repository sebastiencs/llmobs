import { createFileRoute, notFound, Outlet } from "@tanstack/react-router";
import { $api } from "@/api/client";
import { problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { projectInit, projectQuery } from "@/api/queries";
import { ProjectProvider } from "@/components/shell/project-context";
import { ProjectSidebar } from "@/components/shell/project-sidebar";

export const Route = createFileRoute("/$org/$project")({
  loader: async ({ context, params }) => {
    try {
      await context.queryClient.ensureQueryData(projectQuery(params.org, params.project));
    } catch (error) {
      if (problemOf(error)?.status === 404) {
        throw notFound();
      }
      throw error;
    }
  },
  component: ProjectLayout,
});

function ProjectLayout() {
  const { org, project } = Route.useParams();
  const { data } = $api.useSuspenseQuery(
    "get",
    "/api/v1/projects/{project}",
    projectInit(org, project),
  );

  return (
    <ProjectProvider value={{ org, project, ref: projectRef(org, project), data }}>
      <ProjectSidebar>
        <Outlet />
      </ProjectSidebar>
    </ProjectProvider>
  );
}
