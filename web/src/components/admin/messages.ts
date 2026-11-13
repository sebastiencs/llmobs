import { problemMessage, problemOf } from "@/api/problem";

/** Text for a failed PATCH or reset-link call on one account. */
export function adminUserError(error: unknown): string {
  const problem = problemOf(error);
  switch (problem?.detail) {
    case "cannot change your own account":
      return "You can't change your own account.";
    case "the user is the last owner of an org":
      return "This user is the last owner of an organization. Make someone else an owner first.";
    case "the instance would have no active administrator":
      return "This would leave the instance without an active admin.";
    case "the user is disabled":
      return "This account is disabled. Enable it first.";
    default:
      break;
  }
  if (problem?.status === 404) {
    return "This user no longer exists.";
  }
  return problemMessage(error, "The change failed. Try again.");
}
