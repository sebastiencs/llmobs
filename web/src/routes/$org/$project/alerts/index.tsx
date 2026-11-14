import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { BellRing, Plus, TriangleAlert } from "lucide-react";
import { problemOf } from "@/api/problem";
import { alertRulesQuery } from "@/api/queries-alerts";
import { StateBadge } from "@/components/alerts/state-badge";
import { DisabledReason } from "@/components/disabled-reason";
import { EmptyState } from "@/components/empty-state";
import { Forbidden } from "@/components/forbidden";
import { useProject } from "@/components/shell/project-context";
import { TimeText } from "@/components/time-text";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { conditionSummary, formatAlertValue } from "@/lib/alerts";
import { canManageAlerts } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/alerts/")({
  component: AlertsPage,
});

const CANNOT_MANAGE = "Only project members and above can manage alerts";

function AlertsPage() {
  const { org, project, data } = useProject();
  const query = useQuery(alertRulesQuery(org, project));

  if (query.error) {
    if (problemOf(query.error)?.status === 403) {
      return <Forbidden description="You don't have access to this project's alert rules." />;
    }
    throw query.error;
  }

  const rules = query.data?.data ?? [];
  const empty = !query.isPending && rules.length === 0;
  const newRule = canManageAlerts(data.role) ? (
    <Button asChild>
      <Link to="/$org/$project/alerts/new" params={{ org, project }}>
        <Plus />
        New rule
      </Link>
    </Button>
  ) : (
    <DisabledReason reason={CANNOT_MANAGE}>
      <Button disabled>
        <Plus />
        New rule
      </Button>
    </DisabledReason>
  );

  return (
    <div className="mx-auto flex w-full max-w-5xl flex-1 flex-col gap-6 overflow-auto px-4 py-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="flex flex-col gap-1">
          <h1 className="text-lg font-medium">Alerts</h1>
          <p className="text-sm text-muted-foreground">
            Rules are evaluated every minute, or every 5 minutes for windows of 1 hour or more.
          </p>
        </div>
        {empty ? null : newRule}
      </div>
      {query.isPending ? (
        <>
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </>
      ) : empty ? (
        <>
          <EmptyState
            icon={BellRing}
            title="No alert rules yet"
            description="Alert on error rate, cost, latency or a score."
          />
          <div className="flex justify-center">{newRule}</div>
        </>
      ) : (
        <Table aria-label="Alert rules">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>State</TableHead>
              <TableHead>Condition</TableHead>
              <TableHead>Last value</TableHead>
              <TableHead>Last evaluated</TableHead>
              <TableHead>Channels</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rules.map((rule) => (
              <TableRow key={rule.id}>
                <TableCell>
                  <Link
                    to="/$org/$project/alerts/$ruleId"
                    params={{ org, project, ruleId: rule.id }}
                    className="font-medium underline-offset-4 hover:underline"
                  >
                    {rule.name}
                  </Link>
                </TableCell>
                <TableCell>
                  <div className="flex items-center gap-2">
                    <StateBadge state={rule.state} enabled={rule.enabled} />
                    {rule.last_error ? (
                      <span title={rule.last_error} role="img" aria-label="Evaluation failed">
                        <TriangleAlert aria-hidden="true" className="size-4 text-destructive" />
                      </span>
                    ) : null}
                  </div>
                </TableCell>
                <TableCell>{conditionSummary(rule.condition)}</TableCell>
                <TableCell className="tabular-nums">
                  {formatAlertValue(rule.condition, rule.last_value)}
                </TableCell>
                <TableCell>
                  {rule.last_evaluated_at ? (
                    <TimeText value={rule.last_evaluated_at} style="datetime" />
                  ) : (
                    "Never"
                  )}
                </TableCell>
                <TableCell>{rule.channel_ids.length}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </div>
  );
}
