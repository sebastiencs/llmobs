import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, RouterProvider } from "@tanstack/react-router";
import { render } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ThemeProvider } from "@/components/theme-provider";
import { createAppRouter } from "@/router";

export async function renderApp(url: string) {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: Infinity, staleTime: Infinity },
    },
  });
  const router = createAppRouter({
    queryClient,
    history: createMemoryHistory({ initialEntries: [url] }),
  });
  const view = render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  await router.load();
  return { router, user: userEvent.setup(), queryClient, ...view };
}
