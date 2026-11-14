// SPDX-License-Identifier: Elastic-2.0
import { useQuery } from "@tanstack/react-query";
import { TriangleAlert } from "lucide-react";
import { adminLicenseQuery } from "@/api/queries-admin";
import type { Schema } from "@/api/types";
import { Skeleton } from "@/components/ui/skeleton";

const FEATURE_NAMES: Record<Schema<"Feature">, string> = {
  project_roles: "Per-project roles",
  sso_enforcement: "SSO enforcement",
};

/** The licence on the admin settings page: status, customer, id and the features it lists. */
export function LicenseCard() {
  const license = useQuery(adminLicenseQuery);
  if (license.isPending) {
    return <Skeleton className="h-24 w-full" />;
  }
  if (license.error) {
    throw license.error;
  }
  const data = license.data;

  return (
    <section aria-label="Licence" className="flex flex-col gap-2">
      <h3 className="text-base font-medium">Licence</h3>
      <dl className="grid grid-cols-[minmax(10rem,max-content)_1fr] gap-x-6 gap-y-2 text-sm">
        <dt className="text-muted-foreground">Status</dt>
        <dd>
          {data.status === "grace" ? (
            <TriangleAlert
              className="mr-1 inline size-4 align-text-bottom text-destructive"
              aria-hidden="true"
            />
          ) : null}
          {statusLabel(data)}
        </dd>
        {data.customer ? (
          <>
            <dt className="text-muted-foreground">Customer</dt>
            <dd>{data.customer}</dd>
          </>
        ) : null}
        {data.id ? (
          <>
            <dt className="text-muted-foreground">Licence id</dt>
            <dd className="font-mono">{data.id}</dd>
          </>
        ) : null}
        <dt className="text-muted-foreground">Features</dt>
        <dd>
          {data.features.length > 0
            ? data.features.map((feature) => FEATURE_NAMES[feature]).join(", ")
            : "None"}
        </dd>
      </dl>
      <p className="text-sm text-muted-foreground">
        Set the key with LLMOBS_LICENSE_KEY and restart the server.
      </p>
    </section>
  );
}

function statusLabel(license: Schema<"AdminLicense">): string {
  switch (license.status) {
    case "none":
      return "No licence key. Enterprise features are locked.";
    case "invalid":
      return `The licence key was refused: ${license.reason ?? "no reason given"}.`;
    case "active":
      return `Active until ${day(license.expires_at)}`;
    case "grace":
      return `Expired on ${day(license.expires_at)}. Enterprise features stop on ${day(license.grace_ends_at)}.`;
    case "expired":
      return `Expired on ${day(license.expires_at)}. Enterprise features are locked.`;
  }
}

/** The UTC day of an RFC 3339 instant; licence instants are whole UTC days. */
function day(iso: string | undefined): string {
  return iso?.slice(0, 10) ?? "";
}
