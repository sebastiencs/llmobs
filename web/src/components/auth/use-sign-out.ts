import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { clearUserData } from "@/api/auth";
import { $api } from "@/api/client";

/** POST /api/auth/logout, then /login. A failed logout still leaves the page. */
export function useSignOut(): { signOut: () => void; pending: boolean } {
  const logout = $api.useMutation("post", "/api/auth/logout");
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  return {
    pending: logout.isPending,
    signOut: () => {
      logout.mutate(
        {},
        {
          onSettled: () => {
            void navigate({ to: "/login" }).then(() => {
              clearUserData(queryClient);
            });
          },
        },
      );
    },
  };
}
