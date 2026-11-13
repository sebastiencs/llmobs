import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

export function TextField({
  id,
  label,
  value,
  onChange,
  type = "text",
  error,
  description,
  autoComplete,
  required,
  disabled,
  readOnly,
  maxLength,
  placeholder,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  type?: "text" | "email" | "password" | "number";
  error?: string;
  description?: string;
  autoComplete?: string;
  required?: boolean;
  disabled?: boolean;
  readOnly?: boolean;
  maxLength?: number;
  placeholder?: string;
}) {
  const describedBy = [description ? `${id}-description` : null, error ? `${id}-error` : null]
    .filter((item): item is string => item !== null)
    .join(" ");
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      <Input
        id={id}
        name={id}
        type={type}
        value={value}
        onChange={(event) => {
          onChange(event.target.value);
        }}
        aria-invalid={error ? true : undefined}
        aria-describedby={describedBy.length > 0 ? describedBy : undefined}
        autoComplete={autoComplete}
        required={required}
        disabled={disabled}
        readOnly={readOnly}
        maxLength={maxLength}
        placeholder={placeholder}
      />
      {description ? (
        <p id={`${id}-description`} className="text-xs text-muted-foreground">
          {description}
        </p>
      ) : null}
      {error ? (
        <p id={`${id}-error`} className="text-xs text-destructive">
          {error}
        </p>
      ) : null}
    </div>
  );
}
