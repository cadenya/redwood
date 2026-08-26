/** Error model. The API reports failures as a google.rpc.Status payload. */

export interface ErrorStatus {
  code?: number;
  message?: string;
  details?: Array<Record<string, unknown>>;
}

export class APIError extends Error {
  readonly status: number;
  readonly code: number | undefined;
  readonly details: Array<Record<string, unknown>> | undefined;

  constructor(status: number, body: ErrorStatus | undefined, message?: string) {
    super(message ?? body?.message ?? `HTTP ${status}`);
    this.name = 'APIError';
    this.status = status;
    this.code = body?.code;
    this.details = body?.details;
  }
}

export class APIConnectionError extends Error {
  constructor(cause: unknown) {
    super('Connection error', { cause });
    this.name = 'APIConnectionError';
  }
}

export class APIUserAbortError extends Error {
  constructor() {
    super('Request was aborted');
    this.name = 'APIUserAbortError';
  }
}

/**
 * The request could not be constructed locally (unserializable body, invalid
 * argument). No network attempt was made and the call is never retried.
 */
export class APIRequestError extends Error {
  constructor(message: string, cause: unknown) {
    super(message, { cause });
    this.name = 'APIRequestError';
  }
}

/** The configured request deadline elapsed before the response completed. */
export class APITimeoutError extends Error {
  constructor(timeoutMs: number) {
    super(`Request timed out after ${timeoutMs}ms`);
    this.name = 'APITimeoutError';
  }
}

/**
 * The server answered outside the declared protocol: an empty/null body
 * where a JSON document was promised, malformed JSON, or a 204 on an
 * output-bearing operation.
 */
export class APIResponseError extends Error {
  readonly status: number;

  constructor(status: number, message: string, cause?: unknown) {
    super(message, cause === undefined ? undefined : { cause });
    this.name = 'APIResponseError';
    this.status = status;
  }
}
