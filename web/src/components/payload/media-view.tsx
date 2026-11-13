import { useState } from "react";
import type { Schema } from "@/api/types";
import { MediaChip } from "@/components/payload/media-chip";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { classifyMedia } from "@/lib/media";

export function MediaView({
  part,
  projectRef,
}: {
  part: Schema<"UriPart"> | Schema<"BlobPart">;
  projectRef?: string;
}) {
  const mime = typeof part.mime_type === "string" ? part.mime_type : undefined;
  const modality = typeof part.modality === "string" ? part.modality : undefined;
  if (part.type === "blob") {
    const src = blobUrl(part);
    if (isImage(modality, mime)) return <ImageMedia src={src} />;
    if (isAudio(modality, mime)) return <AudioControl src={src} />;
    return <MediaChip uri={src} mimeType={mime} projectRef={projectRef} />;
  }
  const source = classifyMedia(projectRef, part.uri);
  if (source.kind === "dropped") {
    return <p className="text-xs text-muted-foreground">{droppedLabel(part.size)}</p>;
  }
  if (source.kind === "external") {
    return <ExternalMedia uri={source.url} mime={mime} modality={modality} />;
  }
  if (source.kind === "stored" || source.kind === "inline") {
    if (isImage(modality, mime)) return <ImageMedia src={source.url} />;
    if (isAudio(modality, mime)) return <AudioControl src={source.url} />;
  }
  return <MediaChip uri={part.uri} mimeType={mime} projectRef={projectRef} />;
}

function ExternalMedia({ uri, mime, modality }: { uri: string; mime?: string; modality?: string }) {
  const [loaded, setLoaded] = useState(false);
  if (!loaded) {
    return (
      <span className="inline-flex items-center gap-2 rounded bg-muted px-1 text-xs text-muted-foreground">
        <span className="break-all">{uri}</span>
        <button type="button" className="text-foreground" onClick={() => setLoaded(true)}>
          Load
        </button>
      </span>
    );
  }
  if (isImage(modality, mime)) return <ImageMedia src={uri} />;
  if (isAudio(modality, mime)) return <AudioControl src={uri} />;
  return <MediaChip uri={uri} mimeType={mime} />;
}

function AudioControl({ src }: { src: string }) {
  return (
    // biome-ignore lint/a11y/useMediaCaption: ingested audio has no captions
    <audio controls preload="none" src={src} />
  );
}

function ImageMedia({ src }: { src: string }) {
  return (
    <Dialog>
      <DialogTrigger asChild>
        <button type="button" className="block" aria-label="Expand image">
          <img alt="" src={src} loading="lazy" className="max-h-40 max-w-60 object-contain" />
        </button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-[min(90vw,64rem)]">
        <DialogHeader>
          <DialogTitle className="sr-only">Image</DialogTitle>
          <DialogDescription className="sr-only">Full size image</DialogDescription>
        </DialogHeader>
        <img alt="" src={src} className="max-h-[80vh] max-w-full object-contain" />
      </DialogContent>
    </Dialog>
  );
}

function blobUrl(part: Schema<"BlobPart">): string {
  const mime =
    typeof part.mime_type === "string" && part.mime_type.length > 0
      ? part.mime_type
      : "application/octet-stream";
  return `data:${mime};base64,${part.content}`;
}

function isImage(modality: string | undefined, mime: string | undefined): boolean {
  if (modality === "image") return true;
  return typeof mime === "string" && mime.startsWith("image/");
}

function isAudio(modality: string | undefined, mime: string | undefined): boolean {
  if (modality === "audio") return true;
  return typeof mime === "string" && mime.startsWith("audio/");
}

function droppedLabel(size: unknown): string {
  if (typeof size !== "number" || !Number.isFinite(size)) {
    return "Media dropped at ingest (too large)";
  }
  return `Media dropped at ingest (too large, ${megabytes(size)} MB)`;
}

function megabytes(bytes: number): string {
  const mb = bytes / 1_000_000;
  return mb >= 10 ? mb.toFixed(0) : mb.toFixed(1);
}
