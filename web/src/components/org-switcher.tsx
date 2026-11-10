import { useNavigate, useParams } from "@tanstack/react-router";
import { $api } from "@/api/client";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

export function OrgSwitcher() {
  const { org: current } = useParams({ from: "/$org" });
  const navigate = useNavigate();
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  const membership = me.orgs.find((org) => org.slug === current);

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" aria-label={membership?.name ?? current}>
          {membership?.name ?? current}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start">
        <DropdownMenuRadioGroup
          value={current}
          onValueChange={(slug) => {
            void navigate({ to: "/$org", params: { org: slug } });
          }}
        >
          {me.orgs.map((org) => (
            <DropdownMenuRadioItem key={org.id} value={org.slug}>
              {org.name}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
