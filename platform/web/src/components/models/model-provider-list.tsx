import { ChevronRight } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCard,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { modelProviderDefinition } from "./catalog";
import type { ConnectedModelProvider, ModelProviderStatus } from "./use-model-providers";

/// Connected model providers; a row opens the details dialog.
export function ModelProviderList({
  providers,
  onSelect,
}: {
  providers: ConnectedModelProvider[];
  onSelect: (provider: ConnectedModelProvider) => void;
}) {
  if (providers.length === 0) {
    return (
      <p className="rounded-xl border border-dashed p-5 text-sm text-muted-foreground">
        No model providers yet. Use <span className="font-medium">Add provider</span> to connect an
        API key, a compatible endpoint, or a coding-agent subscription.
      </p>
    );
  }
  return (
    <TableCard>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Provider</TableHead>
            <TableHead>Type</TableHead>
            <TableHead>Status</TableHead>
            <TableHead>Updated</TableHead>
            <TableHead className="w-0" />
          </TableRow>
        </TableHeader>
        <TableBody>
          {providers.map((provider) => {
            const { Logo } = modelProviderDefinition(provider.kind);
            return (
              <TableRow
                key={provider.id}
                className="cursor-pointer"
                onClick={() => onSelect(provider)}
              >
                <TableCell>
                  <div className="flex items-center gap-3">
                    <span className="shrink-0 text-foreground">
                      <Logo size={20} />
                    </span>
                    <div className="grid gap-0.5">
                      <span className="font-medium">{provider.title}</span>
                      <span className="text-xs text-muted-foreground">{provider.subtitle}</span>
                    </div>
                  </div>
                </TableCell>
                <TableCell className="text-muted-foreground">{provider.type}</TableCell>
                <TableCell>
                  <StatusBadge status={provider.status} />
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {formatTimestamp(provider.updatedAtMs)}
                </TableCell>
                <TableCell className="text-muted-foreground">
                  <ChevronRight className="size-4" />
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </TableCard>
  );
}

function StatusBadge({ status }: { status: ModelProviderStatus }) {
  if (status === "active") return <Badge variant="secondary">active</Badge>;
  if (status === "disabled") return <Badge variant="outline">disabled</Badge>;
  return (
    <Badge variant="outline" className="border-destructive/50 text-destructive">
      needs attention
    </Badge>
  );
}

function formatTimestamp(timestampMs: number): string {
  if (!timestampMs) return "—";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(timestampMs));
}
