/** `{project}` path value. openapi-fetch encodes the slash. */
export function projectRef(org: string, project: string): string {
  return `${org}/${project}`;
}
