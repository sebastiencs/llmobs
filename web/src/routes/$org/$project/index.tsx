import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/$org/$project/")({
  beforeLoad: ({ params }) => {
    throw redirect({ to: "/$org/$project/traces", params });
  },
});
