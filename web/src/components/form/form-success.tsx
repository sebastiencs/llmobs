export function FormSuccess({ message }: { message?: string }) {
  if (!message) {
    return null;
  }
  return (
    <p role="status" className="text-sm text-muted-foreground">
      {message}
    </p>
  );
}
