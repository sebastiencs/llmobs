import { Link } from "@tanstack/react-router";
import { AuthLayout } from "@/components/auth/auth-layout";
import { AUTH_LINK_CLASS } from "@/components/auth/link-class";
import { ResendVerificationForm } from "@/components/auth/resend-verification";

/** The screen after a sign-up that must verify its email address. */
export function CheckEmail({ email, smtpEnabled }: { email: string; smtpEnabled: boolean }) {
  return (
    <AuthLayout
      title="Check your email"
      description={
        smtpEnabled ? (
          <>
            We sent a verification link to <strong>{email}</strong>. Open it to finish creating your
            account.
          </>
        ) : (
          <>
            Your account <strong>{email}</strong> must be verified before you can sign in, but email
            is not configured on this server. Ask an instance admin to verify your account.
          </>
        )
      }
      footer={
        <Link to="/login" className={AUTH_LINK_CLASS}>
          Back to sign in
        </Link>
      }
    >
      {smtpEnabled ? <ResendVerificationForm email={email} /> : null}
    </AuthLayout>
  );
}
