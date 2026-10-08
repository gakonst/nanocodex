import * as Menu from "@radix-ui/react-dropdown-menu";
import { Check, ChevronDown, ChevronRight, Zap } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router";
import type { ManagedCreateSettings } from "nanocodex/managed";
import { useAccountSession } from "./AccountSession";
import { useAccountQuery } from "./useAccountQuery";
import { decodeModelCatalog } from "./modelCatalog";
import { clientFailureMessage } from "./clientFailure";

type Model = ManagedCreateSettings["model"];
type Thinking = ManagedCreateSettings["thinking"];
const efforts: readonly [Thinking, string][] = [
  ["none", "None"],
  ["low", "Low"],
  ["medium", "Medium"],
  ["high", "High"],
  ["xhigh", "Extra high"],
  ["max", "Maximum"],
];

export function AgentModelMenu({
  agentReady,
  managed = true,
  modelLocked,
  settings,
  onFastMode,
  onModel,
  onThinking,
}: {
  agentReady: boolean;
  managed?: boolean;
  modelLocked: boolean;
  settings: ManagedCreateSettings;
  onFastMode(enabled: boolean): Promise<unknown>;
  onModel(model: Model, normalized: Pick<ManagedCreateSettings, "thinking" | "fastMode" | "reasoningMode">): Promise<unknown>;
  onThinking(thinking: Thinking): Promise<unknown>;
}) {
  const accountId = useAccountSession().account?.id;
  const { query, refresh } = useAccountQuery(accountId, "/v1/models", decodeModelCatalog, { staleTime: 0 });
  useEffect(() => {
    const changed = () => { void refresh(); };
    window.addEventListener("nanocodex:model-credential-changed", changed);
    return () => window.removeEventListener("nanocodex:model-credential-changed", changed);
  }, [refresh]);
  const models = (query.error ? [] : query.data?.models ?? []).filter(model => managed || model.provider === "openai");
  const claude = query.error ? undefined : query.data?.claude;
  const selected = models.find(model => model.id === settings.model);
  const effortPinned = modelLocked && settings.model.startsWith("claude-");
  const [error, setError] = useState<string>();
  const [pending, setPending] = useState(false);
  async function run(operation: () => Promise<unknown>) {
    if (pending) return;
    setPending(true);
    setError(undefined);
    try {
      await operation();
    } catch (cause) {
      setError(
        clientFailureMessage(cause, "Couldn’t update the model. Try again."),
      );
    } finally {
      setPending(false);
    }
  }
  const modelName =
    selected?.name ?? settings.model;
  const effortName =
    efforts.find(([id]) => id === settings.thinking)?.[1] ?? settings.thinking;
  return (
    <div className="agent-runtime-controls">
      <Menu.Root onOpenChange={(open) => { if (open) void refresh(); }}>
        <Menu.Trigger
          className="agent-model-trigger"
          disabled={!agentReady || pending}
          aria-label={`Model settings: ${modelName}, ${effortName}`}
        >
          {settings.fastMode ? <Zap aria-hidden="true" /> : null}
          <span>{modelName}</span>
          <span className="agent-model-effort">{effortName}</span>
          <ChevronDown aria-hidden="true" />
        </Menu.Trigger>
        <Menu.Portal>
          <Menu.Content
            className="agent-model-menu"
            side="top"
            align="end"
            sideOffset={12}
            collisionPadding={12}
          >
            <Menu.Sub>
              <Menu.SubTrigger className="agent-model-menu-item">
                <span>
                  <strong>{modelName}</strong>
                  <small>
                    {modelLocked
                      ? "Start a new chat to change models"
                      : "Choose a model"}
                  </small>
                </span>
                <ChevronRight />
              </Menu.SubTrigger>
              <Menu.Portal>
                <Menu.SubContent
                  className="agent-model-menu"
                  sideOffset={6}
                  collisionPadding={12}
                >
                  <Menu.Label className="agent-model-menu-label">
                    Model
                  </Menu.Label>
                  <Menu.RadioGroup
                    value={settings.model}
                    onValueChange={(value) =>
                      void run(() => {
                        const choice = models.find(model => model.id === value);
                        if (!choice) throw new Error("This model is no longer available. Refresh your connections.");
                        return onModel(choice.id, {
                          thinking: choice.thinking.includes(settings.thinking) ? settings.thinking : choice.thinking[0],
                          fastMode: choice.fastMode && settings.fastMode,
                          reasoningMode: choice.reasoningModes.includes(settings.reasoningMode) ? settings.reasoningMode : "standard",
                        });
                      })
                    }
                  >
                    {!models.length ? <Menu.Label className="agent-model-menu-label">
                      {query.error ? "Couldn’t load available models" : query.isPending ? "Loading available models…" : "Connect a model subscription in account settings"}
                    </Menu.Label> : null}
                    {models.map(({id, name}) => (
                      <Menu.RadioItem
                        className="agent-model-menu-item"
                        key={id}
                        value={id}
                        disabled={modelLocked || pending}
                      >
                        <span>{name}</span>
                        <Menu.ItemIndicator>
                          <Check />
                        </Menu.ItemIndicator>
                      </Menu.RadioItem>
                    ))}
                  </Menu.RadioGroup>
                </Menu.SubContent>
              </Menu.Portal>
            </Menu.Sub>
            {managed && claude && !claude.available ? <>
              <Menu.Separator className="agent-model-menu-separator" />
              {claude.connected ? <Menu.Label className="agent-model-menu-label">
                {claude.error ? "Couldn’t load Claude models. Reopen to retry." : "No Claude models are available for this subscription."}
              </Menu.Label> : null}
              <Menu.Item asChild className="agent-model-menu-item">
                <Link to="/account#claude-connection">{claude.connected ? "Manage Claude connection" : "Connect Claude"}</Link>
              </Menu.Item>
            </> : null}
            <Menu.Separator className="agent-model-menu-separator" />
            <Menu.Sub>
              <Menu.SubTrigger className="agent-model-menu-item">
                <span>Thinking</span>
                <span className="agent-model-fast">
                  {effortName}
                  <ChevronRight />
                </span>
              </Menu.SubTrigger>
              <Menu.Portal>
                <Menu.SubContent
                  className="agent-model-menu"
                  sideOffset={6}
                  collisionPadding={12}
                >
                  <Menu.Label className="agent-model-menu-label">
                    {effortPinned ? "Thinking fixed for this Claude conversation" : "Thinking"}
                  </Menu.Label>
                  <Menu.RadioGroup
                    value={settings.thinking}
                    onValueChange={(value) =>
                      void run(() => {
                        if (effortPinned) throw new Error("Thinking is fixed for this Claude conversation. Start a new chat to change it.");
                        return onThinking(value as Thinking);
                      })
                    }
                  >
                    {efforts.map(([id, label]) => (
                      <Menu.RadioItem
                        className="agent-model-menu-item"
                        key={id}
                        value={id}
                        disabled={
                          pending || effortPinned ||
                          !selected?.thinking.includes(id)
                        }
                      >
                        <span>{label}</span>
                        <Menu.ItemIndicator>
                          <Check />
                        </Menu.ItemIndicator>
                      </Menu.RadioItem>
                    ))}
                  </Menu.RadioGroup>
                </Menu.SubContent>
              </Menu.Portal>
            </Menu.Sub>
            <Menu.Separator className="agent-model-menu-separator" />
            <Menu.CheckboxItem
              className="agent-model-menu-item"
              checked={settings.fastMode}
              disabled={pending || !selected?.fastMode}
              onCheckedChange={(value) => void run(() => onFastMode(value))}
            >
              <span className="agent-model-fast">
                <Zap />
                Fast mode
              </span>
              <Menu.ItemIndicator>
                <Check />
              </Menu.ItemIndicator>
            </Menu.CheckboxItem>
          </Menu.Content>
        </Menu.Portal>
      </Menu.Root>
      {error || query.error ? (
        <p className="agent-model-error" role="alert">
          {error ?? "Couldn’t load available models. Reopen model settings to retry."}
        </p>
      ) : null}
    </div>
  );
}
