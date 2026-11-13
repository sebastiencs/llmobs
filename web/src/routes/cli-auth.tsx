import { createFileRoute, useRouter } from "@tanstack/react-router";
import { Loader2 } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage } from "@/api/problem";
import { AuthLayout } from "@/components/auth/auth-layout";
import { FormError } from "@/components/form/form-error";
import { Button } from "@/components/ui/button";
import { cliAuthSearchFrom, cliCallbackUrl, cliTokenName, parseCliRequest } from "@/lib/cli-auth";
import { navigateExternal } from "@/lib/external-navigation";
import { PRODUCT_NAME } from "@/product";

export const Route = createFileRoute("/cli-auth")({
  component: CliAuthPage,
});

function CliAuthPage() {
  const router = useRouter();
  // The raw query string: the router JSON-parses and re-serialises search values
  // (README § Conventions, Search params), which could change the CLI's values.
  const request = parseCliRequest(cliAuthSearchFrom(router.history.location.search));
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  // gcTime 0: the response holds the secret, which is never shown here (D-L-5).
  const create = $api.useMutation("post", "/api/v1/me/tokens", { gcTime: 0 });
  const [outcome, setOutcome] = useState<"pending" | "authorized" | "cancelled">("pending");

  if (request === undefined) {
    return (
      // The empty fragment satisfies AuthLayout's required children; these states
      // have no body of their own.
      <AuthLayout
        title="This sign-in link is invalid"
        description="Run the login command in your terminal again to get a new link."
      >
        <></>
      </AuthLayout>
    );
  }

  if (outcome === "authorized") {
    return (
      <AuthLayout
        title="You're signed in"
        description="Return to your terminal. You can close this tab. If the terminal is still waiting, run the login command again and revoke the extra token on your account page."
      >
        <></>
      </AuthLayout>
    );
  }

  if (outcome === "cancelled") {
    return (
      <AuthLayout
        title="Sign-in cancelled"
        description="Nothing was created. You can close this tab."
      >
        <></>
      </AuthLayout>
    );
  }

  const authorize = () => {
    create.mutate(
      { body: { name: cliTokenName(request), scopes: ["read", "write"] } },
      {
        onSuccess: (data) => {
          const url = cliCallbackUrl(request, { token: data.secret });
          create.reset();
          setOutcome("authorized");
          navigateExternal(url);
        },
      },
    );
  };

  const cancel = () => {
    setOutcome("cancelled");
    navigateExternal(cliCallbackUrl(request, { error: "access_denied" }));
  };

  return (
    <AuthLayout
      title={`Authorize the ${PRODUCT_NAME} CLI`}
      description={
        <>
          Signed in as <strong>{me.user.email}</strong>.
        </>
      }
    >
      <p className="text-sm">
        The CLI on <strong>{request.host ?? "this computer"}</strong> will get a personal access
        token named <strong>{cliTokenName(request)}</strong> with read and write access. You can
        revoke it on your account page.
      </p>
      <FormError
        message={
          create.error
            ? problemMessage(create.error, "Could not create the token. Try again.")
            : undefined
        }
      />
      <div className="flex gap-2">
        <Button type="button" disabled={create.isPending} onClick={authorize}>
          {create.isPending ? <Loader2 className="animate-spin" aria-hidden="true" /> : null}
          Authorize
        </Button>
        <Button type="button" variant="outline" disabled={create.isPending} onClick={cancel}>
          Cancel
        </Button>
      </div>
    </AuthLayout>
  );
}
