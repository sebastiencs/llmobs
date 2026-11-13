import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/$org/$project/settings/")({
  beforeLoad: ({ params }) => {
    throw redirect({ to: "/$org/$project/settings/general", params });
  },
});
