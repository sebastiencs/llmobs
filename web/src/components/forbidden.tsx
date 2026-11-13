import { Link } from "@tanstack/react-router";

export function Forbidden({
  title = "You don't have access to this page",
  description,
}: {
  title?: string;
  description: string;
}) {
  return (
    <div className="mx-auto flex max-w-md flex-col gap-3 px-4 py-16">
      <h1 className="text-lg font-medium">{title}</h1>
      <p className="text-sm text-muted-foreground">{description}</p>
      <Link
        to="/"
        className="rounded-sm text-sm underline-offset-4 outline-none hover:underline focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2 focus-visible:outline-solid"
      >
        Home
      </Link>
    </div>
  );
}
