import { Link } from "@tanstack/react-router";

export function NotFound() {
  return (
    <div className="mx-auto flex max-w-md flex-col gap-3 px-4 py-16">
      <h1 className="text-lg font-medium">Page not found</h1>
      <Link
        to="/"
        className="rounded-sm text-sm underline-offset-4 outline-none hover:underline focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2 focus-visible:outline-solid"
      >
        Home
      </Link>
    </div>
  );
}
