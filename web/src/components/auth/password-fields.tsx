import { TextField } from "@/components/form/text-field";

export type PasswordProblems = { password?: string; confirm?: string };

/**
 * D-L-11: length in Unicode scalar values after NFC normalization, as the server counts
 * (10 to 128), and both fields equal.
 */
export function passwordProblems(password: string, confirm: string): PasswordProblems {
  const problems: PasswordProblems = {};
  const length = [...password.normalize("NFC")].length;
  if (length < 10) {
    problems.password = "Use at least 10 characters.";
  } else if (length > 128) {
    problems.password = "Use at most 128 characters.";
  }
  if (confirm !== password) {
    problems.confirm = "The passwords do not match.";
  }
  return problems;
}

export function PasswordFields({
  password,
  confirm,
  onPasswordChange,
  onConfirmChange,
  passwordError,
  confirmError,
  disabled,
  passwordLabel = "Password",
}: {
  password: string;
  confirm: string;
  onPasswordChange: (value: string) => void;
  onConfirmChange: (value: string) => void;
  passwordError?: string;
  confirmError?: string;
  disabled?: boolean;
  passwordLabel?: string;
}) {
  return (
    <>
      <TextField
        id="password"
        label={passwordLabel}
        type="password"
        autoComplete="new-password"
        required
        value={password}
        onChange={onPasswordChange}
        description="10 to 128 characters."
        error={passwordError}
        disabled={disabled}
      />
      <TextField
        id="password_confirm"
        label="Confirm password"
        type="password"
        autoComplete="new-password"
        required
        value={confirm}
        onChange={onConfirmChange}
        error={confirmError}
        disabled={disabled}
      />
    </>
  );
}
