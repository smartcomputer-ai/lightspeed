import { PlainSessionController, type ControllerPresentationProps } from "@session-ui/controller-presentation";
import { useActiveUniverse } from "@/lib/universes";
import { useAvailableWebModules } from "@/lib/web-modules";

/** Only an available module can read its controller's data and add links to its pages. */
export function ModuleSessionController(props: ControllerPresentationProps) {
  const { universe } = useActiveUniverse();
  const { modules } = useAvailableWebModules(universe?.id === props.universeId ? universe : undefined);
  const { session } = props;
  const contribution = session && modules.find(module => module.sessionController?.matches(session))?.sessionController;
  const Presenter = contribution?.Presenter ?? PlainSessionController;
  return <Presenter {...props} />;
}
