import * as Menu from "@radix-ui/react-dropdown-menu";
import { Check, ChevronDown, ChevronRight, Zap } from "lucide-react";
import { useState } from "react";
import type { ManagedCreateSettings } from "nanocodex/managed";
import { clientFailureMessage } from "./clientFailure";

type Model = ManagedCreateSettings["model"];
type Thinking = ManagedCreateSettings["thinking"];
const models: readonly [Model, string][] = [
  ["gpt-6-astra", "GPT-6 Astra"],
  ["gpt-5.6-sol", "GPT-5.6 Sol"],
  ["gpt-5.6-terra", "GPT-5.6 Terra"],
  ["gpt-5.6-luna", "GPT-5.6 Luna"],
];
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
  modelLocked,
  settings,
  onFastMode,
  onModel,
  onThinking,
}: {
  agentReady: boolean;
  modelLocked: boolean;
  settings: ManagedCreateSettings;
  onFastMode(enabled: boolean): Promise<unknown>;
  onModel(model: Model): Promise<unknown>;
  onThinking(thinking: Thinking): Promise<unknown>;
}) {
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
    models.find(([id]) => id === settings.model)?.[1] ?? settings.model;
  const effortName =
    efforts.find(([id]) => id === settings.thinking)?.[1] ?? settings.thinking;
  return (
    <div className="agent-runtime-controls">
      <Menu.Root>
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
                      void run(() => onModel(value as Model))
                    }
                  >
                    {models.map(([id, label]) => (
                      <Menu.RadioItem
                        className="agent-model-menu-item"
                        key={id}
                        value={id}
                        disabled={modelLocked || pending}
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
                    Thinking
                  </Menu.Label>
                  <Menu.RadioGroup
                    value={settings.thinking}
                    onValueChange={(value) =>
                      void run(() => onThinking(value as Thinking))
                    }
                  >
                    {efforts.map(([id, label]) => (
                      <Menu.RadioItem
                        className="agent-model-menu-item"
                        key={id}
                        value={id}
                        disabled={
                          pending ||
                          (settings.model === "gpt-6-astra" && id === "none")
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
              disabled={pending}
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
      {error ? (
        <p className="agent-model-error" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}
