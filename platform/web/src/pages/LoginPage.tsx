import { useEffect, useRef, useState, type FormEvent } from "react";
import { authClient, useLoginConfig } from "@/auth";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { beginAutomaticSignIn } from "@/lib/automatic-sign-in";

export function LoginPage() {
  const config = useLoginConfig();
  const [emergency, setEmergency] = useState(false);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(() => new URLSearchParams(window.location.search).has("error") ? "Company sign-in failed or access was not granted. Try again or contact your administrator." : null);
  const [busy, setBusy] = useState(false);
  const automaticSignInStarted = useRef(false);

  const companySignIn = async () => {
    if (!config.data?.providerId) return;
    setBusy(true);
    setError(null);
    try {
      const result = await authClient.signIn.social({
        provider: config.data.providerId,
        callbackURL: new URL(import.meta.env.BASE_URL, window.location.origin).href,
        errorCallbackURL: new URL(`${import.meta.env.BASE_URL}login`, window.location.origin).href,
      });
      if (result.error) throw new Error("Company sign-in is unavailable. Try again or contact your administrator.");
    } catch (error) {
      setError(error instanceof Error ? error.message : "Sign-in failed");
      setBusy(false);
    }
  };

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    if (config.data?.sso && config.data.autoSignIn && config.data.providerId && (params.has("auto") || params.has("expired")) &&
        !params.has("error") && !automaticSignInStarted.current && beginAutomaticSignIn()) {
      automaticSignInStarted.current = true;
      void companySignIn();
    }
  }, [config.data]);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const result = await authClient.signIn.email({ email, password });
      if (result.error) throw new Error(result.error.message ?? "Sign-in failed");
      window.location.assign(import.meta.env.BASE_URL);
    } catch (error) {
      setError(error instanceof Error ? error.message : "Sign-in failed");
      setBusy(false);
    }
  };

  return (
    <div className="flex min-h-svh items-center justify-center p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle className="text-lg">Lightspeed Platform</CardTitle>
          <CardDescription>{emergency ? "Local emergency administrator access." : "Sign in to your account."}</CardDescription>
        </CardHeader>
        <CardContent>
          {config.isPending && <p>Loading sign-in settings…</p>}
          {config.error && <p role="alert" className="text-sm text-destructive">{config.error.message}</p>}
          {config.data?.sso && !emergency && <div className="grid gap-4">
            <Button disabled={busy} onClick={companySignIn}>{busy ? "Redirecting…" : "Sign in with your company account"}</Button>
            {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
            {config.data.password === "break-glass" && <Button variant="link" size="xs" className="justify-self-center font-normal text-muted-foreground/60 hover:text-muted-foreground" onClick={() => setEmergency(true)}>Admins</Button>}
          </div>}
          {config.data && config.data.password !== "off" && (!config.data.sso || emergency) && <>
          <form onSubmit={submit} className="grid gap-4">
            <Field>
              <FieldLabel htmlFor="email">Email</FieldLabel>
              <Input
                id="email"
                type="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                autoComplete="username"
                required
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="password">Password</FieldLabel>
              <Input
                id="password"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                autoComplete="current-password"
                required
              />
            </Field>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <Button type="submit" className="w-full" disabled={busy}>
              {busy ? "Signing in…" : "Sign in"}
            </Button>
            <p className="text-center text-xs text-muted-foreground">
              {config.data.sso ? "Only designated local emergency administrators can use a password." : "Accounts are invite-only."}
            </p>
          </form>
          {emergency && <Button variant="link" onClick={() => setEmergency(false)}>Back to company sign-in</Button>}
          </>}
        </CardContent>
      </Card>
    </div>
  );
}
