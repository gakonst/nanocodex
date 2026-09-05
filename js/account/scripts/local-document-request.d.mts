export interface LocalDocumentRequest {
  readonly method?: string;
  readonly headers: {
    readonly accept?: string;
    readonly "sec-fetch-dest"?: string;
    readonly "sec-fetch-mode"?: string;
  };
}

export function isLocalDocumentRequest(
  request: LocalDocumentRequest,
  allowGeneric?: boolean,
): boolean;
