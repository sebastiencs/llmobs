import { AudioLines, File, FileText, Image as ImageIcon } from "lucide-react";
import type { ReactNode } from "react";
import { useOptionalProject } from "@/components/shell/project-context";
import { mediaUrl } from "@/lib/media";

export function MediaChip({
  uri,
  mimeType,
  projectRef,
}: {
  uri: string;
  mimeType?: string | null;
  projectRef?: string;
}) {
  const project = useOptionalProject();
  const ref = projectRef ?? project?.ref;
  const href = ref ? mediaUrl(ref, uri) : undefined;
  const mime = mimeType || "file";
  const body = (
    <span className="inline-flex items-center gap-1 rounded bg-muted px-1 text-muted-foreground">
      <MediaIcon mime={mime} />
      <span>{mime}</span>
      <span className="font-mono">{chipLabel(uri)}</span>
    </span>
  );
  if (!href) return body;
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="rounded-sm outline-none focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2 focus-visible:outline-solid"
    >
      {body}
    </a>
  );
}

function chipLabel(uri: string): string {
  if (uri.startsWith("media://")) return uri.slice("media://".length).slice(0, 8);
  if (uri.startsWith("data:")) return "inline";
  return uri;
}

function MediaIcon({ mime }: { mime: string }): ReactNode {
  if (mime.startsWith("image/")) return <ImageIcon className="size-3" aria-hidden />;
  if (mime.startsWith("audio/")) return <AudioLines className="size-3" aria-hidden />;
  if (mime === "application/pdf") return <FileText className="size-3" aria-hidden />;
  return <File className="size-3" aria-hidden />;
}
