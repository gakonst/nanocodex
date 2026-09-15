import type { NamedTool, ToolContext } from "nanocodex";
import {
  USER_DATA_TOOL_DESCRIPTION,
  isUserDataMutation,
  parseUserDataOperation,
  userDataToolInputSchema,
  type UserDataOperation,
} from "nanocodex-tools/user-data";

export type UserDataToolOptions = Readonly<{
  execute(operation: UserDataOperation): Promise<unknown>;
  requireCapability(capability: "data:read" | "data:write", context: ToolContext): void;
}>;

export function userDataTool(options: UserDataToolOptions): NamedTool {
  return {
    name: "user_data",
    description: USER_DATA_TOOL_DESCRIPTION,
    parameters: userDataToolInputSchema(),
    handler: async (input: unknown, context: ToolContext) => {
      context.signal.throwIfAborted();
      const operation = parseUserDataOperation(input);
      options.requireCapability(isUserDataMutation(operation) ? "data:write" : "data:read", context);
      const result = await options.execute(operation);
      context.signal.throwIfAborted();
      return result;
    },
  };
}
