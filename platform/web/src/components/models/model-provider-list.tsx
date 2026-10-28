import { EmptyState } from "@/components/page";
import { BrainCircuit } from "lucide-react";
import { TableCell } from "@/components/ui/table";
import { ResourceIdentityCell, ResourceRow, ResourceStatus, ResourceTable, ResourceValue } from "@/components/resource-table";
import { modelProviderDefinition } from "./catalog";
import type { ConnectedModelProvider } from "./use-model-providers";

export function ModelProviderList({ providers, onSelect, showEmpty = true }: {
  providers: ConnectedModelProvider[];
  onSelect: (provider: ConnectedModelProvider, trigger: HTMLButtonElement | null) => void;
  showEmpty?: boolean;
}) {
  if (providers.length === 0) {
    return showEmpty ? (
      <EmptyState icon={BrainCircuit} title="No model providers yet">
        Provider API keys, compatible endpoints and coding-agent subscriptions that sessions call models through.
      </EmptyState>
    ) : null;
  }
  return (
    <ResourceTable label="Model providers" columns={["Provider", "Connection", "Status"]}>
      {providers.map((provider) => {
        const { Logo } = modelProviderDefinition(provider.kind);
        return (
          <ResourceRow key={provider.id} name={provider.title} onOpen={(trigger) => onSelect(provider, trigger)}>
            <ResourceIdentityCell name={provider.title} subtitle={provider.subtitle} mono={provider.subtitleMono} icon={<Logo size={20} />} compactDescription={[provider.connection, provider.connectionDescription, provider.subtitle.trim().toLocaleLowerCase() !== provider.title.trim().toLocaleLowerCase() ? provider.subtitle : null].filter(Boolean).join(" · ")} />
            <TableCell className="hidden max-w-56 sm:table-cell"><ResourceValue description={provider.connectionDescription}>{provider.connection}</ResourceValue></TableCell>
            <TableCell><ResourceStatus status={provider.status} /></TableCell>
          </ResourceRow>
        );
      })}
    </ResourceTable>
  );
}
