import { type ErrorComponentProps, useRouter } from "@tanstack/react-router";
import { problemOf } from "@/api/problem";
import { Button } from "@/components/ui/button";

export function ErrorState({ error, reset }: ErrorComponentProps) {
  const router = useRouter();
  const problem = problemOf(error);
  const signedOut = problem?.status === 401;
  const title = signedOut ? "You are signed out" : (problem?.title ?? "Something went wrong");
  const detail = signedOut ? "Sign in again to continue." : problem?.detail;

  return (
    <div className="mx-auto flex max-w-md flex-col gap-3 px-4 py-16">
      <h1 className="text-lg font-medium">{title}</h1>
      {detail ? <p>{detail}</p> : null}
      {problem && !signedOut ? <p className="font-mono text-xs">{problem.code}</p> : null}
      <Button
        type="button"
        className="w-fit"
        onClick={() => {
          void router.invalidate();
          reset();
        }}
      >
        Try again
      </Button>
    </div>
  );
}
