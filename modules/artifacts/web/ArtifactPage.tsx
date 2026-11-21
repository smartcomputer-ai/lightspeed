import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { formatUploadBytes } from "@lightspeed-ai/platform-shared";
import { useActiveUniverse, useDocumentTitle } from "@lightspeed-ai/platform-web/modules";
import { BlobDownloadLink, BlobPreview, blobView } from "@lightspeed-ai/platform-web/modules/blobs";
import { LoadingNote, PageHeader, ReadError, UniverseNotFound } from "@lightspeed-ai/platform-web/modules/ui";
import { publication, publicationContent } from "./queries";

export function ArtifactPage() {
  const { universe, handle, isLoading } = useActiveUniverse();
  const { artifactId } = useParams();
  if (isLoading) return <LoadingNote />;
  if (!universe || !artifactId) return <UniverseNotFound handle={handle} />;
  return <Snapshot key={`${universe.id}/${artifactId}`} universeId={universe.id} id={artifactId} handle={universe.handle} />;
}

function Snapshot({ universeId, id, handle }: { universeId: string; id: string; handle: string }) {
  const record = useQuery(publication(universeId, id));
  const artifact = record.data?.artifact;
  const content = useQuery({ ...publicationContent(universeId, id), enabled: record.isSuccess });
  const bytes = useMemo(() => content.data &&
    Uint8Array.from(atob(content.data.bytesBase64), character => character.charCodeAt(0)), [content.data]);
  const view = useMemo(() => bytes && blobView(bytes, content.data?.mediaType ?? undefined), [bytes, content.data]);
  useDocumentTitle([artifact?.title]);
  return <section className="mx-auto w-full max-w-4xl p-4 sm:p-6">
    <Link to={`/u/${encodeURIComponent(handle)}/artifacts`} className="mb-4 inline-block text-sm underline underline-offset-4">All artifacts</Link>
    {record.error ? <ReadError error={record.error} /> : !artifact ? <LoadingNote /> : <>
      <PageHeader title={artifact.title} description={
        <>Published {new Date(artifact.createdAt).toLocaleDateString()} from {artifact.workspaceId} / {artifact.path} · {formatUploadBytes(artifact.sizeBytes)}</>
      } actions={!content.error && bytes && <BlobDownloadLink bytes={bytes} name={artifact.path.split("/").at(-1)!} />} />
      {content.error ? <ReadError error={content.error} /> : bytes && view ?
        <div className="overflow-hidden rounded-lg border"><BlobPreview bytes={bytes} view={view} name={artifact.title} /></div> : <LoadingNote />}
    </>}
  </section>;
}
