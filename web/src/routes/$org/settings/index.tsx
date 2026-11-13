import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/$org/settings/")({
  beforeLoad: ({ params }) => {
    throw redirect({ to: "/$org/settings/general", params });
  },
});
