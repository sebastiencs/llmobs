import { useMatch, useNavigate, useParams } from "@tanstack/react-router";
import { Home, Search } from "lucide-react";
import { useEffect, useState } from "react";
import { $api } from "@/api/client";
import { projectListInit } from "@/api/queries";
import { readRecent } from "@/components/command/recent-traces";
import { ShortcutsHelp } from "@/components/command/shortcuts-help";
import { projectNav } from "@/components/shell/nav";
import { useTheme } from "@/components/theme-provider";
import { Button } from "@/components/ui/button";
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { setTimeZone, useTimeZone } from "@/lib/settings";
import {
  type Chord,
  dispatchShortcut,
  goDestination,
  hexId,
  isElementShortcut,
  isTypingTarget,
  reduceShortcut,
} from "@/lib/shortcuts";

const CHORD_MS = 1000;

const THEMES = [
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
  { value: "system", label: "System" },
] as const;

function isOverlayOpen(): boolean {
  return (
    document.querySelector(
      "[role='dialog'][data-state='open'], [role='menu'][data-state='open'], [data-slot='popover-content'][data-state='open']",
    ) !== null
  );
}

export function CommandPalette() {
  const [palette, setPalette] = useState(false);
  const [help, setHelp] = useState(false);
  const [query, setQuery] = useState("");
  const navigate = useNavigate();
  const { org } = useParams({ from: "/$org" });
  const project = useMatch({ from: "/$org/$project", shouldThrow: false })?.params.project;
  const { theme, setTheme } = useTheme();
  const zone = useTimeZone();
  const projects = $api.useInfiniteQuery(
    "get",
    "/api/v1/orgs/{org}/projects",
    projectListInit(org),
    {
      pageParamName: "cursor",
      initialPageParam: null,
      getNextPageParam: (page) => page.next_cursor ?? undefined,
    },
  );
  const projectRows = projects.data?.pages[0]?.data ?? [];
  const traceId = project ? hexId(query, 32) : undefined;
  const spanId = project && !traceId ? hexId(query, 16) : undefined;
  const recent = project ? readRecent(org, project) : [];

  useEffect(() => {
    let chord: Chord = null;
    let timer = 0;
    function onKeyDown(event: KeyboardEvent) {
      const next = reduceShortcut(chord, event, isTypingTarget(event.target));
      chord = next.chord;
      window.clearTimeout(timer);
      if (chord) {
        timer = window.setTimeout(() => {
          chord = null;
        }, CHORD_MS);
      }
      const action = next.action;
      if (!action || isElementShortcut(action)) {
        return;
      }
      if (action === "palette") {
        event.preventDefault();
        setPalette((open) => !open);
        return;
      }
      if (action === "help") {
        event.preventDefault();
        setHelp(true);
        return;
      }
      if (action === "dismiss") {
        if (isOverlayOpen()) {
          return;
        }
        dispatchShortcut("dismiss", event);
        if (
          document.activeElement instanceof HTMLElement &&
          document.activeElement !== document.body
        ) {
          document.activeElement.blur();
        }
        return;
      }
      const destination = goDestination(action);
      if (destination) {
        if (!project) {
          return;
        }
        event.preventDefault();
        void navigate({ to: destination, params: { org, project } });
        return;
      }
      dispatchShortcut(action, event);
    }
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.clearTimeout(timer);
    };
  }, [navigate, org, project]);

  function closePalette() {
    setPalette(false);
    setQuery("");
  }

  return (
    <>
      <Button
        type="button"
        variant="outline"
        className="h-7 w-36 justify-between px-2 text-xs font-normal text-muted-foreground"
        aria-label="Command palette"
        aria-keyshortcuts="Meta+K Control+K"
        onClick={() => setPalette(true)}
      >
        <span className="inline-flex items-center gap-1.5">
          <Search />
          Search
        </span>
        <kbd className="font-mono text-[10px]">⌘K</kbd>
      </Button>
      <CommandDialog
        open={palette}
        onOpenChange={(open) => {
          setPalette(open);
          if (!open) {
            setQuery("");
          }
        }}
        title="Command palette"
        description="Jump to a page, project, or trace."
      >
        <Command>
          <CommandInput
            placeholder="Search or paste an id"
            value={query}
            onValueChange={setQuery}
          />
          <CommandList>
            <CommandEmpty>No matches</CommandEmpty>
            {project && traceId ? (
              <CommandGroup heading="Open trace">
                <CommandItem
                  value={query}
                  onSelect={() => {
                    closePalette();
                    void navigate({
                      to: "/$org/$project/traces/$traceId",
                      params: { org, project, traceId },
                    });
                  }}
                >
                  Open trace {traceId}
                </CommandItem>
              </CommandGroup>
            ) : null}
            {project && spanId ? (
              <CommandGroup heading="Find span">
                <CommandItem
                  value={query}
                  onSelect={() => {
                    closePalette();
                    void navigate({
                      to: "/$org/$project/spans",
                      params: { org, project },
                      search: { filter: `span_id = '${spanId}'` },
                    });
                  }}
                >
                  Find span {spanId}
                </CommandItem>
              </CommandGroup>
            ) : null}
            <CommandGroup heading="Go to">
              {project
                ? projectNav.map((item) => (
                    <CommandItem
                      key={item.to}
                      value={item.label}
                      onSelect={() => {
                        closePalette();
                        void navigate({ to: item.to, params: { org, project } });
                      }}
                    >
                      <item.icon />
                      {item.label}
                    </CommandItem>
                  ))
                : null}
              <CommandItem
                value="Home"
                onSelect={() => {
                  closePalette();
                  void navigate({ to: "/$org", params: { org } });
                }}
              >
                <Home />
                Home
              </CommandItem>
            </CommandGroup>
            {projectRows.length > 0 ? (
              <CommandGroup heading="Projects">
                {projectRows.map((item) => (
                  <CommandItem
                    key={item.id}
                    value={item.name}
                    onSelect={() => {
                      closePalette();
                      void navigate({
                        to: "/$org/$project/traces",
                        params: { org, project: item.slug },
                      });
                    }}
                  >
                    {item.name}
                  </CommandItem>
                ))}
              </CommandGroup>
            ) : null}
            {recent.length > 0 ? (
              <CommandGroup heading="Recent traces">
                {recent.map((item) => (
                  <CommandItem
                    key={item.id}
                    value={`${item.name} ${item.id}`}
                    onSelect={() => {
                      if (!project) {
                        return;
                      }
                      closePalette();
                      void navigate({
                        to: "/$org/$project/traces/$traceId",
                        params: { org, project, traceId: item.id },
                      });
                    }}
                  >
                    {item.name}
                  </CommandItem>
                ))}
              </CommandGroup>
            ) : null}
            <CommandGroup heading="Theme">
              {THEMES.map((option) => (
                <CommandItem
                  key={option.value}
                  value={`${option.label} theme`}
                  data-checked={theme === option.value}
                  onSelect={() => {
                    setTheme(option.value);
                    closePalette();
                  }}
                >
                  {option.label}
                </CommandItem>
              ))}
            </CommandGroup>
            <CommandGroup heading="Time zone">
              <CommandItem
                value={zone === "utc" ? "Show times in local time" : "Show times in UTC"}
                onSelect={() => {
                  setTimeZone(zone === "utc" ? "local" : "utc");
                  closePalette();
                }}
              >
                {zone === "utc" ? "Show times in local time" : "Show times in UTC"}
              </CommandItem>
            </CommandGroup>
          </CommandList>
        </Command>
      </CommandDialog>
      <ShortcutsHelp open={help} onOpenChange={setHelp} />
    </>
  );
}
