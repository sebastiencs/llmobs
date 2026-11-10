import { Link } from "@tanstack/react-router";

export function NotFound() {
  return (
    <div className="mx-auto flex max-w-md flex-col gap-3 px-4 py-16">
      <h1 className="text-lg font-medium">Page not found</h1>
      <Link to="/" className="text-sm underline-offset-4 hover:underline">
        Home
      </Link>
    </div>
  );
}
