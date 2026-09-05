import type { AgentEvent, TurnUsage } from "nanocodex";

export type PromptRequest = {
  id: string;
  input: string;
};

export type TurnCompleted = {
  type: "turn_completed";
  id: string;
  final_message: string;
  usage: TurnUsage;
};

export type SessionEvent =
  | {
      type: "ready";
      session_id: string;
      restored: boolean;
    }
  | {
      type: "turn_accepted";
      id: string;
      input: string;
      replayed: boolean;
    }
  | {
      type: "event";
      turn_id: string;
      event: AgentEvent;
    }
  | TurnCompleted
  | {
      type: "turn_failed";
      id: string;
      error: string;
    };

export type TurnOutcome =
  | {
      ok: true;
      completed: TurnCompleted;
    }
  | {
      ok: false;
      error: string;
    };

export type StreamRecord = {
  type: "stream_event";
  index: number;
  event: SessionEvent;
};
