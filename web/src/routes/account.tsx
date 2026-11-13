import { createFileRoute } from "@tanstack/react-router";
import { PasswordForm } from "@/components/account/password-form";
import { ProfileForm } from "@/components/account/profile-form";
import { TopBarLayout } from "@/components/shell/top-bar-layout";

export const Route = createFileRoute("/account")({
  component: AccountPage,
});

function AccountPage() {
  return (
    <TopBarLayout>
      <main className="mx-auto flex w-full max-w-3xl flex-1 flex-col gap-10 overflow-auto px-4 py-6">
        <h1 className="text-2xl font-medium">Account</h1>
        <section className="flex flex-col gap-3" aria-labelledby="profile-heading">
          <h2 id="profile-heading" className="text-lg font-medium">
            Profile
          </h2>
          <ProfileForm />
        </section>
        <section className="flex flex-col gap-3" aria-labelledby="password-heading">
          <h2 id="password-heading" className="text-lg font-medium">
            Password
          </h2>
          <PasswordForm />
        </section>
      </main>
    </TopBarLayout>
  );
}
