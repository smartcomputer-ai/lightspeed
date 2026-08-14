import { useState, type FormEvent } from "react";
import { useMutation } from "@tanstack/react-query";
import { Monitor, Moon, Sun } from "lucide-react";
import { authClient, type SessionUser } from "@/auth";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { PageHeader } from "@/components/page";
import { useTheme, type Theme } from "@/lib/theme";

export function AccountPage({ user }: { user: SessionUser }) {
  return (
    <>
      <PageHeader title="Account" description="Your profile, password, and appearance." />
      <div className="grid gap-6">
        <ProfileCard user={user} />
        <PasswordCard />
        <ThemeCard />
      </div>
    </>
  );
}

function ProfileCard({ user }: { user: SessionUser }) {
  const [name, setName] = useState(user.name);

  const save = useMutation({
    mutationFn: async () => {
      const result = await authClient.updateUser({ name: name.trim() });
      if (result.error) {
        throw new Error(result.error.message ?? "failed to update profile");
      }
    },
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    save.mutate();
  };

  return (
    <Card id="profile" className="scroll-mt-6">
      <CardHeader>
        <CardTitle>Profile</CardTitle>
      </CardHeader>
      <CardContent>
        <form onSubmit={submit} className="grid max-w-md gap-4">
          <Field>
            <FieldLabel htmlFor="account-email">Email</FieldLabel>
            <Input id="account-email" value={user.email} disabled />
          </Field>
          <Field>
            <FieldLabel htmlFor="account-name">Name</FieldLabel>
            <Input
              id="account-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
            />
          </Field>
          {save.error && <p className="text-sm text-destructive">{save.error.message}</p>}
          {save.isSuccess && <p className="text-sm text-muted-foreground">Saved.</p>}
          <div>
            <Button
              type="submit"
              disabled={save.isPending || name.trim() === user.name || !name.trim()}
            >
              {save.isPending ? "Saving…" : "Save"}
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  );
}

function PasswordCard() {
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState<string | null>(null);

  const change = useMutation({
    mutationFn: async () => {
      const result = await authClient.changePassword({
        currentPassword,
        newPassword,
        revokeOtherSessions: true,
      });
      if (result.error) {
        throw new Error(result.error.message ?? "failed to change password");
      }
    },
    onSuccess: () => {
      setCurrentPassword("");
      setNewPassword("");
      setConfirm("");
      setError(null);
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    if (newPassword !== confirm) {
      setError("New passwords do not match.");
      return;
    }
    change.mutate();
  };

  return (
    <Card id="security" className="scroll-mt-6">
      <CardHeader>
        <CardTitle>Password</CardTitle>
        <CardDescription>Other sessions are signed out after a change.</CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={submit} className="grid max-w-md gap-4">
          <Field>
            <FieldLabel htmlFor="current-password">Current password</FieldLabel>
            <Input
              id="current-password"
              type="password"
              value={currentPassword}
              onChange={(e) => setCurrentPassword(e.target.value)}
              autoComplete="current-password"
              required
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="new-password">New password</FieldLabel>
            <Input
              id="new-password"
              type="password"
              value={newPassword}
              onChange={(e) => setNewPassword(e.target.value)}
              autoComplete="new-password"
              minLength={8}
              required
            />
            <FieldDescription>At least 8 characters.</FieldDescription>
          </Field>
          <Field>
            <FieldLabel htmlFor="confirm-password">Repeat new password</FieldLabel>
            <Input
              id="confirm-password"
              type="password"
              value={confirm}
              onChange={(e) => setConfirm(e.target.value)}
              autoComplete="new-password"
              required
            />
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          {change.isSuccess && !error && (
            <p className="text-sm text-muted-foreground">Password changed.</p>
          )}
          <div>
            <Button type="submit" disabled={change.isPending}>
              {change.isPending ? "Changing…" : "Change password"}
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  );
}

function ThemeCard() {
  const { theme, setTheme } = useTheme();
  const options: { value: Theme; label: string; icon: typeof Sun }[] = [
    { value: "light", label: "Light", icon: Sun },
    { value: "dark", label: "Dark", icon: Moon },
    { value: "system", label: "System", icon: Monitor },
  ];

  return (
    <Card id="appearance" className="scroll-mt-6">
      <CardHeader>
        <CardTitle>Appearance</CardTitle>
      </CardHeader>
      <CardContent className="flex gap-2">
        {options.map(({ value, label, icon: Icon }) => (
          <Button
            key={value}
            variant={theme === value ? "secondary" : "outline"}
            onClick={() => setTheme(value)}
          >
            <Icon data-icon="inline-start" />
            {label}
          </Button>
        ))}
      </CardContent>
    </Card>
  );
}
