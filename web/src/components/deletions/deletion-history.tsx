import { History } from "lucide-react";
import { useId } from "react";
import { $api } from "@/api/client";
import { problemMessage } from "@/api/problem";
import { deletionsInit } from "@/api/queries-settings";
import { EmptyState } from "@/components/empty-state";
import { FormError } from "@/components/form/form-error";
import { IdText } from "@/components/id-text";
import { useProject } from "@/components/shell/project-context";
import { TimeText } from "@/components/time-text";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  deletionErrorLabel,
  deletionKindLabel,
  deletionPollMs,
  deletionStatusLabel,
  deletionSubject,
  isOpenDeletion,
} from "@/lib/deletions";

/** The project's deletions, newest first. Refetches while one is open. */
export function DeletionHistory() {
  const { org, project } = useProject();
  const headingId = useId();
  const list = $api.useQuery(
    "get",
    "/api/v1/projects/{project}/deletions",
    deletionsInit(org, project),
    {
      refetchInterval: (query) =>
        query.state.data?.data.some(isOpenDeletion) ? deletionPollMs() : false,
    },
  );

  const rows = list.data?.data ?? [];

  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-3">
      <h2 id={headingId} className="text-base font-medium">
        Deletion history
      </h2>
      {list.data === undefined && list.error ? (
        <FormError message={problemMessage(list.error, "Could not load the deletions.")} />
      ) : null}
      {list.data === undefined ? null : (
        <>
          {list.error ? (
            <FormError message={problemMessage(list.error, "Could not load the deletions.")} />
          ) : null}
          {rows.length === 0 ? (
            <EmptyState icon={History} title="No deletions yet" />
          ) : (
            <Table aria-label="Deletions">
              <TableHeader>
                <TableRow>
                  <TableHead>Requested</TableHead>
                  <TableHead>What</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>By</TableHead>
                  <TableHead>Applied</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((deletion) => (
                  <TableRow key={deletion.id}>
                    <TableCell>
                      <TimeText value={deletion.created_at} style="datetime" />
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-col">
                        <span>{deletionKindLabel(deletion.kind)}</span>
                        <span className="text-xs text-muted-foreground">
                          {deletionSubject(deletion)}
                        </span>
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-col items-start gap-1">
                        <Badge
                          variant={
                            deletion.status === "failed"
                              ? "destructive"
                              : deletion.status === "applied"
                                ? "secondary"
                                : "outline"
                          }
                        >
                          {deletionStatusLabel(deletion.status)}
                        </Badge>
                        {deletion.error ? (
                          <span className="text-xs text-muted-foreground">
                            {deletionErrorLabel(deletion.error)}
                          </span>
                        ) : null}
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-col">
                        <span>
                          {deletion.requested_by.kind === "api_key" ? "API key" : "Member"}
                        </span>
                        <IdText
                          value={deletion.requested_by.id}
                          kind={deletion.requested_by.kind === "api_key" ? "API key" : "user"}
                        />
                      </div>
                    </TableCell>
                    <TableCell>
                      {deletion.applied_at ? (
                        <TimeText value={deletion.applied_at} style="datetime" />
                      ) : (
                        "—"
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </>
      )}
    </section>
  );
}
