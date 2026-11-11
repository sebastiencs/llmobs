import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TimeText } from "@/components/time-text";
import { getTimeZone, setTimeZone, TIME_ZONE_STORAGE_KEY, useTimeZone } from "@/lib/settings";

function ZoneProbe() {
  const zone = useTimeZone();
  return (
    <>
      <span>{zone}</span>
      <TimeText value="2026-06-01T12:00:00.000000000Z" style="time" />
    </>
  );
}

describe("time zone setting", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    setTimeZone("local");
  });

  it("defaults to local and ignores an invalid stored value", () => {
    expect(getTimeZone()).toBe("local");
    localStorage.setItem(TIME_ZONE_STORAGE_KEY, "nope");
    expect(getTimeZone()).toBe("local");
  });

  it("stores utc and keeps the choice when storage throws", () => {
    setTimeZone("utc");
    expect(localStorage.getItem(TIME_ZONE_STORAGE_KEY)).toBe("utc");
    expect(getTimeZone()).toBe("utc");

    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    expect(() => setTimeZone("local")).not.toThrow();
    expect(getTimeZone()).toBe("local");
  });

  it("re-renders a TimeText when the setting changes", () => {
    render(<ZoneProbe />);
    const time = () => document.querySelector("time");
    expect(screen.getByText("local")).toBeInTheDocument();
    expect(time()).toHaveAttribute("data-zone", "local");

    act(() => {
      setTimeZone("utc");
    });

    expect(screen.getByText("utc")).toBeInTheDocument();
    expect(time()).toHaveAttribute("data-zone", "utc");
    expect(time()).toHaveTextContent("12:00:00.000");
  });
});
