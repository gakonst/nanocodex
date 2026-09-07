export class ConvertError extends Error {
  readonly status: number
  readonly code?: string
  /** Seconds until the client may retry; set on 429 responses. */
  readonly retryAfter?: number

  constructor(status: number, message: string, code?: string, retryAfter?: number) {
    super(message)
    this.name = 'ConvertError'
    this.status = status
    this.code = code
    this.retryAfter = retryAfter
  }
}
