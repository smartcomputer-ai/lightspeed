import { useLocation, useParams } from "react-router-dom";
import { SessionIcon } from "@/icons";
import { sessionDraftKey } from "@session-ui/model/draft";
import { DetailPrompt, LoadingNote, UniverseNotFound } from "@/components/page";
import { useCreateParam } from "@/lib/create-param";
import { ProviderReadinessBanner } from "@/components/provider-readiness-banner";
import { useActionPermissions } from "@/lib/permissions";
import { useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";
import { ListPane } from "@/components/list-pane";
import { SessionDetail } from "@session-ui/SessionDetail";
import { SessionList } from "./session-list";

export function SessionsPage() {
  const { universe, handle, isLoading } = useActiveUniverse();
  const { sessionId } = useParams<{ sessionId: string }>();
  const location = useLocation();
  const canCreate = useActionPermissions(universe?.id).can("createSession");
  const [, setCreateOpen] = useCreateParam("session");

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !universe.role) {
    return (
      <div className="p-6">
        <UniverseNotFound handle={handle} />
      </div>
    );
  }

  return (
    <div className="flex min-h-0 min-w-0 max-w-full flex-1">
      <ListPane detailOpen={Boolean(sessionId)}>
        <SessionList key={universe.id} universeId={universe.id} handle={handle!} activeId={sessionId} />
      </ListPane>
      <section className={cn("min-w-0 flex-1 flex-col", sessionId ? "flex" : "hidden md:flex")}>
        {!sessionId && <ProviderReadinessBanner universeId={universe.id} handle={handle!} />}
        {sessionId ? (
          <SessionDetail
            key={sessionDraftKey(universe.id, sessionId)}
            universeId={universe.id}
            handle={handle!}
            sessionId={sessionId}
            backTo={`/u/${handle}/sessions${location.search}`}
            sessionHref={(target) => `/u/${handle}/sessions/${target}${location.search}`}
          />
        ) : (
          <DetailPrompt
            icon={<SessionIcon className="size-10 text-muted-foreground/60" />}
            create={canCreate ? { label: "New session", onClick: () => setCreateOpen(true) } : undefined}
          >
            Pick a session{canCreate ? ", or start one" : ""}.
          </DetailPrompt>
        )}
      </section>
    </div>
  );
}
