import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { clearUserData } from "@/api/auth";
import { $api } from "@/api/client";
import { setSigningOut } from "@/api/session";

/** POST /api/auth/logout, then /login. A failed logout still leaves the page. */
export function useSignOut(): { signOut: () => void; pending: boolean } {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  // onSettled in the hook options: the navigation unmounts the caller, which would
  // drop a callback passed to `mutate` (README § Pitfalls).
  const logout = $api.useMutation("post", "/api/auth/logout", {
    onSettled: () => {
      void navigate({ to: "/login" }).finally(() => {
        clearUserData(queryClient);
        setSigningOut(false);
      });
    },
  });
  return {
    pending: logout.isPending,
    signOut: () => {
      // From here until the navigation is done, the 401 handler must stay quiet.
      setSigningOut(true);
      logout.mutate({});
    },
  };
}
