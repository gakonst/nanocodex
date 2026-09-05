import { requireTerminalAuthorization } from "@/lib/terminal-auth";
import { errorResponse, parseSessionId } from "@/lib/validation";
import { prepareSessionSandbox } from "@/workflows/session-sandbox";

export const runtime = "nodejs";

export async function POST(
  request: Request,
  context: { params: Promise<{ sessionId: string }> },
): Promise<Response> {
  try {
    requireTerminalAuthorization(request);
    const sessionId = parseSessionId((await context.params).sessionId);
    const sandbox = await prepareSessionSandbox(sessionId);
    const attachment = await sandbox.openInteractive();
    return Response.json(attachment, {
      headers: { "cache-control": "no-store" },
    });
  } catch (error) {
    return errorResponse(error);
  }
}
