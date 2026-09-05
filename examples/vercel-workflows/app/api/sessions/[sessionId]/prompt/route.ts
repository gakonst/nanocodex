import { errorResponse, parsePrompt, parseSessionId } from "@/lib/validation";
import {
  nanocodexPromptHook,
  promptHookToken,
} from "@/workflows/nanocodex-actor";

export const runtime = "nodejs";

export async function POST(
  request: Request,
  context: { params: Promise<{ sessionId: string }> },
): Promise<Response> {
  try {
    const sessionId = parseSessionId((await context.params).sessionId);
    const prompt = parsePrompt(await request.json());
    const resumed = await nanocodexPromptHook.resume(promptHookToken(sessionId), prompt);
    if (!resumed) {
      return Response.json(
        { error: { code: "session_not_ready", message: "session is missing or not accepting prompts" } },
        { status: 409, headers: { "cache-control": "no-store" } },
      );
    }
    return Response.json(
      { accepted: true, session_id: sessionId, turn_id: prompt.id },
      { status: 202, headers: { "cache-control": "no-store" } },
    );
  } catch (error) {
    return errorResponse(error);
  }
}
