import { createContext, type ReactNode, useContext } from "react";
import type { Schema } from "@/api/types";

export type ProjectContextValue = {
  org: string;
  project: string;
  ref: string;
  data: Schema<"Project">;
};

const ProjectContext = createContext<ProjectContextValue | null>(null);

export function ProjectProvider({
  value,
  children,
}: {
  value: ProjectContextValue;
  children: ReactNode;
}) {
  return <ProjectContext.Provider value={value}>{children}</ProjectContext.Provider>;
}

export function useProject(): ProjectContextValue {
  const value = useContext(ProjectContext);
  if (!value) {
    throw new Error("useProject must be used within a project route");
  }
  return value;
}
