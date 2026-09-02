export type ConnectorPathId = "github" | "gmail" | "gdrive" | "gcalendar" | "gtasks"
  | "gdocs" | "gsheets" | "gslides" | "gcontacts" | "slack" | "x";

const ENCODED_GOOGLE_PATH_SEPARATOR = /%(?:2e|2f|5c|25)/i;

export function canonicalConnectorPath(id: ConnectorPathId, pathname: string): boolean {
  return id === "github"
    || (!pathname.includes("\\") && !ENCODED_GOOGLE_PATH_SEPARATOR.test(pathname));
}
