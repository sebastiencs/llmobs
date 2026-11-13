import { useQueryClient } from "@tanstack/react-query";
import { KeyRound, Loader2, MoreHorizontal, Plus } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage, problemOf } from "@/api/problem";
import { personalTokensInit, personalTokensKey } from "@/api/queries-account";
import type { Schema } from "@/api/types";
import { CreateTokenDialog } from "@/components/account/create-token-dialog";
import { accessLabel } from "@/components/account/token-format";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { FormError } from "@/components/form/form-error";
import { SecretDialog } from "@/components/secret-dialog";
import { TimeText } from "@/components/time-text";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

export function PersonalTokens() {
  const queryClient = useQueryClient();
  const tokens = $api.useInfiniteQuery("get", "/api/v1/me/tokens", personalTokensInit(), {
    pageParamName: "cursor",
    initialPageParam: null,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
  });
  const revoke = $api.useMutation("delete", "/api/v1/me/tokens/{id}");
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<Schema<"CreatedPersonalToken"> | undefined>();
  const [revoking, setRevoking] = useState<Schema<"PersonalToken"> | undefined>();

  // A failed first page has no data to show: the route's ErrorState renders it. A failed
  // "Load more" keeps the rows already loaded, so only an inline error is added.
  if (tokens.error && tokens.data === undefined) {
    throw tokens.error;
  }

  const rows = tokens.data?.pages.flatMap((page) => page.data) ?? [];

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center justify-between gap-4">
        <p className="text-sm text-muted-foreground">
          Tokens let the CLI and MCP clients act as you. Treat them like passwords.
        </p>
        <Button type="button" onClick={() => setCreating(true)} className="ml-auto">
          <Plus aria-hidden="true" />
          New token
        </Button>
      </div>
      {tokens.isPending ? (
        <>
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </>
      ) : rows.length === 0 ? (
        <EmptyState
          icon={KeyRound}
          title="No personal access tokens"
          description="Create one to use the CLI or an MCP client."
        />
      ) : (
        <Table aria-label="Personal access tokens">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Prefix</TableHead>
              <TableHead>Access</TableHead>
              <TableHead>Created</TableHead>
              <TableHead>Last used</TableHead>
              <TableHead>Expires</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((token) => (
              <TableRow key={token.id}>
                <TableCell>{token.name}</TableCell>
                <TableCell>
                  <span className="font-mono">{token.prefix}</span>
                </TableCell>
                <TableCell>{accessLabel(token.scopes)}</TableCell>
                <TableCell>
                  <TimeText value={token.created_at} style="datetime" />
                </TableCell>
                <TableCell>
                  {token.last_used_at ? (
                    <TimeText value={token.last_used_at} style="datetime" />
                  ) : (
                    "Never"
                  )}
                </TableCell>
                <TableCell>
                  {token.expires_at ? (
                    <TimeText value={token.expires_at} style="datetime" />
                  ) : (
                    "Never"
                  )}
                </TableCell>
                <TableCell>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        aria-label={`Actions for ${token.name}`}
                      >
                        <MoreHorizontal />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem
                        className="text-destructive"
                        onSelect={() => {
                          setRevoking(token);
                        }}
                      >
                        Revoke
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {tokens.error && tokens.data !== undefined ? (
        <FormError message="Could not load more tokens." />
      ) : null}
      {tokens.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="w-fit"
          disabled={tokens.isFetchingNextPage}
          onClick={() => {
            void tokens.fetchNextPage();
          }}
        >
          {tokens.isFetchingNextPage ? <Loader2 className="animate-spin" /> : null}
          Load more
        </Button>
      ) : null}
      <CreateTokenDialog
        open={creating}
        onOpenChange={setCreating}
        onCreated={(data) => {
          setCreated(data);
          void queryClient.invalidateQueries({ queryKey: personalTokensKey() });
        }}
      />
      {created ? (
        <SecretDialog
          open
          title="Personal access token created"
          secret={created.secret}
          description="Use it as a bearer token in the Authorization header."
          onDone={() => {
            setCreated(undefined);
          }}
        />
      ) : null}
      <ConfirmDialog
        open={revoking !== undefined}
        onOpenChange={(open) => {
          if (!open) {
            setRevoking(undefined);
            revoke.reset();
          }
        }}
        title="Revoke token?"
        description={
          <>
            Anything using <strong>{revoking?.name}</strong> (
            <span className="font-mono">{revoking?.prefix}</span>) stops working at once.
          </>
        }
        confirmLabel="Revoke"
        destructive
        pending={revoke.isPending}
        error={
          revoke.error
            ? problemMessage(revoke.error, "Could not revoke the token. Try again.")
            : undefined
        }
        onConfirm={() => {
          if (!revoking) {
            return;
          }
          revoke.mutate(
            { params: { path: { id: revoking.id } } },
            {
              onSuccess: () => {
                setRevoking(undefined);
                revoke.reset();
                void queryClient.invalidateQueries({ queryKey: personalTokensKey() });
              },
              onError: (error) => {
                // A 404 means the token is already gone: treat it as revoked.
                if (problemOf(error)?.status === 404) {
                  setRevoking(undefined);
                  revoke.reset();
                  void queryClient.invalidateQueries({ queryKey: personalTokensKey() });
                }
              },
            },
          );
        }}
      />
    </div>
  );
}
