import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { THEME_STORAGE_KEY, ThemeProvider, useTheme } from "@/components/theme-provider";
import { setPrefersDark } from "@/test/match-media";

function SchemeHarness() {
  const { resolvedTheme, setTheme } = useTheme();
  return (
    <>
      <button type="button" onClick={() => setTheme("light")}>
        light
      </button>
      <button type="button" onClick={() => setTheme("system")}>
        system
      </button>
      <span>{resolvedTheme}</span>
    </>
  );
}

function Harness({ next = "light" }: { next?: "light" | "dark" }) {
  const { setTheme, theme } = useTheme();
  return (
    <button type="button" onClick={() => setTheme(next)}>
      {theme}
    </button>
  );
}

function renderProvider(next?: "light" | "dark") {
  return render(
    <ThemeProvider>
      <Harness next={next} />
    </ThemeProvider>,
  );
}

describe("ThemeProvider", () => {
  it("defaults to system and follows a dark color scheme", () => {
    setPrefersDark(true);
    renderProvider();
    expect(screen.getByRole("button")).toHaveTextContent("system");
    expect(document.documentElement).toHaveClass("dark");
    expect(document.documentElement).not.toHaveClass("light");
  });

  it("stores light and restores it on the next mount", async () => {
    const user = userEvent.setup();
    const first = renderProvider();
    await user.click(screen.getByRole("button", { name: "system" }));
    expect(document.documentElement).toHaveClass("light");
    expect(document.documentElement).not.toHaveClass("dark");
    expect(document.documentElement.style.colorScheme).toBe("light");
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");

    first.unmount();
    renderProvider();
    expect(screen.getByRole("button", { name: "light" })).toBeInTheDocument();
    expect(document.documentElement).toHaveClass("light");
    expect(document.documentElement.style.colorScheme).toBe("light");
  });

  it("follows scheme changes only while the theme is system", async () => {
    const user = userEvent.setup();
    setPrefersDark(true);
    renderProvider("dark");
    expect(document.documentElement).toHaveClass("dark");

    act(() => {
      setPrefersDark(false);
    });
    expect(document.documentElement).toHaveClass("light");

    await user.click(screen.getByRole("button", { name: "system" }));
    act(() => {
      setPrefersDark(true);
    });
    expect(document.documentElement).toHaveClass("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");
  });

  it("reads the current scheme when returning to system", async () => {
    const user = userEvent.setup();
    setPrefersDark(false);
    render(
      <ThemeProvider>
        <SchemeHarness />
      </ThemeProvider>,
    );
    await user.click(screen.getByRole("button", { name: "light" }));
    expect(document.documentElement).toHaveClass("light");

    setPrefersDark(true, false);
    await user.click(screen.getByRole("button", { name: "system" }));
    expect(screen.getByText("dark")).toBeInTheDocument();
    expect(document.documentElement).toHaveClass("dark");
    expect(document.documentElement).not.toHaveClass("light");
  });

  it("treats an unknown stored value as system", () => {
    localStorage.setItem(THEME_STORAGE_KEY, "nope");
    setPrefersDark(true);
    renderProvider();
    expect(screen.getByRole("button")).toHaveTextContent("system");
    expect(document.documentElement).toHaveClass("dark");
  });

  it("falls back to the default when localStorage throws", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    setPrefersDark(false);
    expect(() => renderProvider()).not.toThrow();
    expect(screen.getByRole("button")).toHaveTextContent("system");
    expect(document.documentElement).toHaveClass("light");
  });
});
