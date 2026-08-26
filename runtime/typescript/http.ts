/**
 * Minimal, dependency-less HTTP core built on global fetch (Node 18+,
 * browsers, Deno, Bun). Handles auth, query serialization, retries with
 * backoff, timeouts, and error mapping.
 */

import {
  APIConnectionError,
  APIError,
  APIRequestError,
  APIResponseError,
  APITimeoutError,
  APIUserAbortError,
  ErrorStatus,
} from './error.js';

export interface RequestOptions {
  /**
   * Auto-reconnect for SSE streams (default true): a mid-stream transport
   * drop resumes from the last received event id, like EventSource. Clean
   * stream end, close(), and abort never reconnect. Set false to surface
   * drops as APIConnectionError instead.
   */
  reconnect?: boolean;
  headers?: Record<string, string>;
  signal?: AbortSignal;
  maxRetries?: number;
  /**
   * Per-request deadline in milliseconds (overrides the client default).
   * Non-finite or <= 0 disables the deadline for this request.
   */
  timeout?: number;
  /**
   * For streaming (SSE) requests: resume after the event with this id by
   * sending it as the Last-Event-ID request header.
   */
  lastEventId?: string;
}

export type QueryPrimitive = string | number | boolean;
export type QueryValue = QueryPrimitive | QueryPrimitive[] | undefined | null;

export interface RequestSpec {
  method: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';
  path: string;
  query?: Record<string, QueryValue>;
  body?: unknown;
  stream?: boolean;
  /** The operation declares no response body (void): 204/empty succeed. */
  void?: boolean;
}

export type LogLevel = 'debug' | 'warn' | 'off';

/** Minimal logger surface; `console` satisfies it. */
export interface Logger {
  debug(...args: unknown[]): void;
  warn(...args: unknown[]): void;
}

export interface HttpClientOptions {
  baseURL: string;
  authHeader: () => Record<string, string>;
  maxRetries?: number;
  /** Deadline for ordinary (non-streaming) requests in ms. Default 60000. */
  timeout?: number;
  defaultHeaders?: Record<string, string>;
  fetch?: typeof fetch;
  /** Client-level default values for prominent params (e.g. a tenant/scope id). */
  defaults?: Record<string, string | undefined>;
  /** Destination for SDK logs. Defaults to `console`. */
  logger?: Logger;
  /**
   * 'debug' logs every request/response line (method, path, status,
   * duration — never headers or bodies); 'warn' (default) logs only
   * retries; 'off' silences the SDK entirely.
   */
  logLevel?: LogLevel;
}

const RETRYABLE_STATUS = new Set([408, 409, 429, 500, 502, 503, 504]);

const DEFAULT_TIMEOUT_MS = 60_000;

/**
 * Encode one path segment, rejecting empty/whitespace values at the boundary
 * — an empty segment would silently rewrite the route (/parents//children).
 */
export function pathSegment(name: string, value: string | undefined): string {
  if (value === undefined || String(value).trim() === '') {
    throw new Error(`Missing required path parameter '${name}'.`);
  }
  return encodeURIComponent(value);
}

/**
 * Snapshot list params at call time so pagination cannot observe later
 * caller mutations (array-valued filters are copied too). Auto-iteration
 * must never combine pages from different result sets.
 */
export function snapshotParams<T>(params: T): T {
  if (params === undefined || params === null || typeof params !== 'object') return params;
  const copy: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(params as Record<string, unknown>)) {
    copy[key] = Array.isArray(value) ? value.slice() : value;
  }
  return copy as T;
}

/**
 * Automatic retries apply only to idempotent methods. A POST/PATCH that
 * succeeds server-side but loses its response would be executed twice if
 * retried; callers can opt a specific mutation in via options.maxRetries.
 */
const IDEMPOTENT_METHODS = new Set(['GET', 'HEAD', 'PUT', 'DELETE']);

/**
 * Settle when the promise settles OR the signal aborts — a custom fetch (or
 * body reader) that ignores AbortSignal must not be able to hold a deadlined
 * request open forever. The orphaned promise's eventual rejection is
 * swallowed; its resolution is discarded.
 */
function raceAbort<T>(promise: Promise<T>, signal: AbortSignal | undefined): Promise<T> {
  if (!signal) return promise;
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => {
      promise.catch(() => {});
      reject(new APIUserAbortError());
    };
    if (signal.aborted) {
      onAbort();
      return;
    }
    signal.addEventListener('abort', onAbort, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener('abort', onAbort);
        resolve(value);
      },
      (err) => {
        signal.removeEventListener('abort', onAbort);
        reject(err);
      },
    );
  });
}

/**
 * Deadline cleanup for streaming responses, handed from rawRequest to the
 * Stream that owns the body. The forwarding listener must survive until the
 * stream terminates (a caller abort has to unblock a pending read on a
 * silent socket), so the Stream — not rawRequest — releases it.
 */
const streamCleanups = new WeakMap<Response, () => void>();

/** Claim (and remove) the cleanup registered for a streaming response. */
export function takeStreamCleanup(response: Response): (() => void) | undefined {
  const cleanup = streamCleanups.get(response);
  streamCleanups.delete(response);
  return cleanup;
}

/** Merged caller-signal + deadline state for one request. */
interface Deadline {
  signal: AbortSignal | undefined;
  /** Stop the timer (body may keep streaming under the caller's signal). */
  settle(): void;
  /**
   * Detach the abort-forwarding listener from the caller's signal. Call
   * only when the request (including any stream body) is finished — a
   * reused long-lived signal must not accumulate one listener per
   * completed call.
   */
  release(): void;
  timedOut(): boolean;
  ms: number;
}

/**
 * A lazily-parsing promise for one API call. Awaiting it (or `.then`) yields
 * the decoded value exactly like a plain promise; `withResponse()` yields the
 * decoded value together with the raw `Response` (status, headers); and
 * `asResponse()` yields the raw `Response` WITHOUT consuming the body, so
 * the caller owns reading it.
 */
export class APIPromise<T> implements Promise<T> {
  private parsed: Promise<T> | undefined;
  private observedRaw = false;

  constructor(
    private readonly responsePromise: Promise<Response>,
    private readonly parseFn: (response: Response) => Promise<T>,
    private readonly onRawAccess: () => void,
  ) {
    // A dropped return value must still reach terminal cleanup: the request
    // has already been sent, so unless raw access claims the body FIRST
    // (synchronously, before any await), parsing starts on the next
    // microtask — consuming the body and settling the deadline timer even
    // when the caller never observes the promise. Rejections on this
    // internal branch are swallowed; a caller who later awaits still gets
    // them from the memoized parse.
    queueMicrotask(() => {
      if (!this.observedRaw) this.parse().catch(() => {});
    });
  }

  /**
   * The raw `Response` after status checking and retries; the body is NOT
   * consumed. Reading the body (and its timing) becomes the caller's
   * responsibility — the request deadline stops at header acquisition.
   */
  asResponse(): Promise<Response> {
    // Must be called synchronously after the request (before any await):
    // it claims body ownership away from the auto-parse safety net. Mixing
    // a LATE asResponse() with parsed access yields a response whose body
    // was already consumed by parsing — status/headers stay usable.
    this.observedRaw = true;
    return this.responsePromise.then((response) => {
      this.onRawAccess();
      return response;
    });
  }

  /** The decoded value together with the `Response` its body came from. */
  async withResponse(): Promise<{ data: T; response: Response }> {
    const response = await this.responsePromise;
    const data = await this.parse();
    return { data, response };
  }

  private parse(): Promise<T> {
    this.parsed ??= this.responsePromise.then(this.parseFn);
    return this.parsed;
  }

  then<TResult1 = T, TResult2 = never>(
    onfulfilled?: ((value: T) => TResult1 | PromiseLike<TResult1>) | null,
    onrejected?: ((reason: unknown) => TResult2 | PromiseLike<TResult2>) | null,
  ): Promise<TResult1 | TResult2> {
    return this.parse().then(onfulfilled, onrejected);
  }

  catch<TResult = never>(
    onrejected?: ((reason: unknown) => TResult | PromiseLike<TResult>) | null,
  ): Promise<T | TResult> {
    return this.parse().catch(onrejected);
  }

  finally(onfinally?: (() => void) | null): Promise<T> {
    return this.parse().finally(onfinally);
  }

  readonly [Symbol.toStringTag] = 'APIPromise';
}

export class HttpClient {
  private readonly baseURL: string;
  private readonly authHeader: () => Record<string, string>;
  private readonly maxRetries: number;
  private readonly timeout: number;
  private readonly defaultHeaders: Record<string, string>;
  private readonly fetchFn: typeof fetch;
  private readonly logger: Logger;
  private readonly logLevel: LogLevel;

  /** Method + path + status only — headers and bodies are never logged. */
  private logDebug(...args: unknown[]): void {
    if (this.logLevel === 'debug') this.logger.debug('[sdk]', ...args);
  }

  private logWarn(...args: unknown[]): void {
    if (this.logLevel !== 'off') this.logger.warn('[sdk]', ...args);
  }
  readonly defaults: Record<string, string | undefined>;

  constructor(options: HttpClientOptions) {
    this.defaults = options.defaults ?? {};
    this.logger = options.logger ?? console;
    this.logLevel = options.logLevel ?? 'warn';
    // Validate the STRUCTURE once: operation paths are appended to this
    // value, so a query/fragment/userinfo would silently swallow the
    // request path. Absolute http(s) with a host is required; a path
    // prefix is supported and kept.
    // A literal delimiter parses as an EMPTY search/hash and slips past
    // the checks below; reject the characters outright.
    if (options.baseURL.includes('?') || options.baseURL.includes('#')) {
      throw new Error(
        `baseURL '${options.baseURL}' must not carry userinfo, query, or fragment`,
      );
    }
    let parsed: URL;
    try {
      parsed = new URL(options.baseURL);
    } catch (err) {
      throw new Error(`baseURL '${options.baseURL}' is not an absolute URL`, { cause: err });
    }
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
      throw new Error(`baseURL '${options.baseURL}' must use http or https`);
    }
    if (parsed.hostname === '') {
      throw new Error(`baseURL '${options.baseURL}' has no host`);
    }
    if (parsed.username !== '' || parsed.password !== '' || parsed.search !== '' || parsed.hash !== '') {
      throw new Error(
        `baseURL '${options.baseURL}' must not carry userinfo, query, or fragment`,
      );
    }
    this.baseURL = (parsed.origin + parsed.pathname).replace(/\/+$/, '');
    this.authHeader = options.authHeader;
    this.maxRetries = options.maxRetries ?? 0;
    this.timeout = options.timeout ?? DEFAULT_TIMEOUT_MS;
    this.defaultHeaders = options.defaultHeaders ?? {};
    this.fetchFn = options.fetch ?? fetch;
  }

  /**
   * The APIPromise pipeline for JSON and void operations. The spec thunk
   * runs inside the async context so synchronous setup failures (e.g. a
   * missing client-default param) surface as rejections, exactly like the
   * plain-promise path. The deadline spans fetch through decode; raw-access
   * consumers release it at header acquisition and own body timing.
   */
  requestAPI<T>(makeSpec: () => RequestSpec, options: RequestOptions = {}): APIPromise<T> {
    const deadline = this.deadline(options);
    let spec: RequestSpec;
    const responsePromise = (async () => {
      try {
        spec = makeSpec();
        return await this.rawRequest(spec, { ...options, signal: deadline.signal });
      } catch (err) {
        deadline.settle();
        deadline.release();
        if (deadline.timedOut()) throw new APITimeoutError(deadline.ms);
        throw err;
      }
    })();
    const parseFn = async (response: Response): Promise<T> => {
      try {
        if (spec.void) {
          void response.body?.cancel().catch(() => {});
          return undefined as T;
        }
        if (response.status === 204) {
          throw new APIResponseError(204, 'HTTP 204 where a JSON response was expected');
        }
        const text = await raceAbort(response.text(), deadline.signal);
        if (text.trim() === '' || text.trim() === 'null') {
          throw new APIResponseError(
            response.status,
            `HTTP ${response.status} with an empty or null body where a JSON response was expected`,
          );
        }
        try {
          return JSON.parse(text) as T;
        } catch (err) {
          throw new APIResponseError(response.status, 'response body is not valid JSON', err);
        }
      } catch (err) {
        if (deadline.timedOut()) throw new APITimeoutError(deadline.ms);
        throw err;
      } finally {
        deadline.settle();
        deadline.release();
      }
    };
    return new APIPromise<T>(responsePromise, parseFn, () => {
      deadline.settle();
      deadline.release();
    });
  }

  async request<T>(spec: RequestSpec, options: RequestOptions = {}): Promise<T> {
    // Ordinary JSON calls keep the deadline through body consumption and
    // decoding — a response that stalls mid-body still times out.
    const deadline = this.deadline(options);
    try {
      const response = await this.rawRequest(spec, { ...options, signal: deadline.signal });
      // Branch on the GENERATED expectation, not the HTTP status: a void
      // method accepts 204/empty, but an output-bearing method requires a
      // JSON document — empty/null would fabricate a resource outside the
      // declared contract, and malformed JSON must be a stable SDK error.
      if (spec.void) {
        void response.body?.cancel().catch(() => {});
        return undefined as T;
      }
      if (response.status === 204) {
        throw new APIResponseError(204, 'HTTP 204 where a JSON response was expected');
      }
      const text = await raceAbort(response.text(), deadline.signal);
      if (text.trim() === '' || text.trim() === 'null') {
        throw new APIResponseError(
          response.status,
          `HTTP ${response.status} with an empty or null body where a JSON response was expected`,
        );
      }
      try {
        return JSON.parse(text) as T;
      } catch (err) {
        throw new APIResponseError(response.status, 'response body is not valid JSON', err);
      }
    } catch (err) {
      if (deadline.timedOut()) throw new APITimeoutError(deadline.ms);
      throw err;
    } finally {
      deadline.settle();
      deadline.release();
    }
  }

  async rawRequest(spec: RequestSpec, options: RequestOptions = {}): Promise<Response> {
    const url = this.buildURL(spec.path, spec.query);

    // One Headers instance; `set` is case-insensitive, so later layers
    // OVERRIDE earlier ones regardless of spelling (object spread would keep
    // both `Authorization` and `authorization` and fetch would join the two
    // values into one corrupt header). Precedence: generated defaults →
    // client defaults → auth → per-request → Last-Event-ID (the semantic
    // option is the single source of resume state).
    const headers = new Headers();
    headers.set('Accept', spec.stream ? 'text/event-stream' : 'application/json');
    if (spec.body !== undefined) headers.set('Content-Type', 'application/json');
    for (const [key, value] of Object.entries(this.defaultHeaders)) headers.set(key, value);
    for (const [key, value] of Object.entries(this.authHeader())) headers.set(key, value);
    for (const [key, value] of Object.entries(options.headers ?? {})) headers.set(key, value);
    if (options.lastEventId !== undefined) {
      headers.set('Last-Event-ID', options.lastEventId);
    }

    // Serialize ONCE, before the transport/retry loop: a circular object or
    // BigInt is a caller bug, not a network failure — it must surface as a
    // stable request-construction error and never be retried.
    let bodyText: string | undefined;
    if (spec.body !== undefined) {
      try {
        bodyText = JSON.stringify(spec.body);
      } catch (err) {
        throw new APIRequestError('request body is not JSON-serializable', err);
      }
      if (bodyText === undefined) {
        throw new APIRequestError('request body serialized to undefined', undefined);
      }
    }

    // Streams bound only connection/response-header acquisition; the body's
    // lifetime stays under the caller's AbortSignal.
    const streamDeadline = spec.stream ? this.deadline(options) : undefined;
    const signal = streamDeadline ? streamDeadline.signal : options.signal;

    // An explicit per-request maxRetries opts in even for mutations.
    const maxRetries = normalizeRetries(
      options.maxRetries !== undefined
        ? options.maxRetries
        : IDEMPOTENT_METHODS.has(spec.method)
          ? this.maxRetries
          : 0,
    );

    const startedAt = Date.now();
    for (let attempt = 0; ; attempt++) {
      this.logDebug(`-> ${spec.method} ${spec.path}`, attempt > 0 ? `(attempt ${attempt + 1})` : '');
      let response: Response;
      try {
        response = await raceAbort(
          this.fetchFn(url, {
            method: spec.method,
            headers,
            body: bodyText,
            signal: signal ?? null,
          }),
          signal,
        );
      } catch (err) {
        if (streamDeadline?.timedOut()) {
          streamDeadline.settle();
          streamDeadline.release();
          throw new APITimeoutError(streamDeadline.ms);
        }
        if (signal?.aborted) {
          streamDeadline?.settle();
          streamDeadline?.release();
          throw new APIUserAbortError();
        }
        if (attempt < maxRetries) {
          this.logWarn(`retrying ${spec.method} ${spec.path} after connection error (attempt ${attempt + 1}/${maxRetries + 1})`);
          await sleep(backoffMs(attempt, undefined), signal);
          if (streamDeadline?.timedOut()) {
            streamDeadline.settle();
            streamDeadline.release();
            throw new APITimeoutError(streamDeadline.ms);
          }
          if (signal?.aborted) throw new APIUserAbortError();
          continue;
        }
        // Terminal connection failure: the deadline timer must not outlive
        // the reported error (an orphaned timer holds the process open).
        streamDeadline?.settle();
        streamDeadline?.release();
        throw new APIConnectionError(err);
      }

      if (response.ok) {
        this.logDebug(`<- ${response.status} ${spec.method} ${spec.path} (${Date.now() - startedAt}ms)`);
        // Response headers acquired: the stream body is now unbounded. The
        // forwarding listener stays attached (caller abort must reach the
        // body); the Stream releases it at its terminal state.
        if (streamDeadline) {
          streamDeadline.settle();
          streamCleanups.set(response, () => streamDeadline.release());
        }
        return response;
      }

      if (RETRYABLE_STATUS.has(response.status) && attempt < maxRetries) {
        this.logWarn(`retrying ${spec.method} ${spec.path} after HTTP ${response.status} (attempt ${attempt + 1}/${maxRetries + 1})`);
        // Release the discarded response so its connection can be reused.
        void response.body?.cancel().catch(() => {});
        await sleep(backoffMs(attempt, response.headers.get('retry-after')), signal);
        // A stream deadline that fired during backoff is a timeout, not a
        // user abort — rawRequest has no outer mapper to reclassify it.
        if (streamDeadline?.timedOut()) {
          streamDeadline.settle();
          streamDeadline.release();
          throw new APITimeoutError(streamDeadline.ms);
        }
        if (signal?.aborted) throw new APIUserAbortError();
        continue;
      }

      let body: ErrorStatus | undefined;
      try {
        body = (await response.json()) as ErrorStatus;
      } catch {
        body = undefined;
      }
      // The deadline stays active while consuming a non-2xx error body; if
      // it expired there, report the timeout rather than a truncated
      // APIError.
      if (streamDeadline?.timedOut()) {
        streamDeadline.settle();
        streamDeadline.release();
        throw new APITimeoutError(streamDeadline.ms);
      }
      streamDeadline?.settle();
      streamDeadline?.release();
      throw new APIError(response.status, body);
    }
  }

  /**
   * Merge the caller's signal with the configured deadline. The caller's
   * abort always forwards; the timer marks timedOut so the thrown error can
   * be classified as APITimeoutError rather than APIUserAbortError.
   */
  private deadline(options: RequestOptions): Deadline {
    const ms = options.timeout ?? this.timeout;
    if (!Number.isFinite(ms) || ms <= 0) {
      return { signal: options.signal, settle() {}, release() {}, timedOut: () => false, ms };
    }
    const controller = new AbortController();
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, ms);
    const forward = () => controller.abort();
    if (options.signal?.aborted) {
      controller.abort();
    } else {
      options.signal?.addEventListener('abort', forward, { once: true });
    }
    let settled = false;
    let released = false;
    return {
      signal: controller.signal,
      settle() {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
      },
      release() {
        if (released) return;
        released = true;
        options.signal?.removeEventListener('abort', forward);
      },
      timedOut: () => timedOut,
      ms,
    };
  }

  private buildURL(path: string, query?: Record<string, QueryValue>): string {
    const url = new URL(this.baseURL + path);
    for (const [key, value] of Object.entries(query ?? {})) {
      if (value === undefined || value === null) continue;
      // Arrays serialize as repeated params: ?state=a&state=b
      for (const item of Array.isArray(value) ? value : [value]) {
        url.searchParams.append(key, String(item));
      }
    }
    return url.toString();
  }
}

/** Retry counts must be bounded non-negative integers; anything else (NaN,
 * Infinity, negatives, fractions) is treated as the nearest sane value. */
function normalizeRetries(value: number): number {
  if (!Number.isFinite(value) || value <= 0) return 0;
  return Math.min(Math.floor(value), 10);
}

function backoffMs(attempt: number, retryAfter: string | null | undefined): number {
  if (retryAfter) {
    // Retry-After is either delta-seconds or an HTTP-date. A parsed zero is
    // a real answer (retry immediately), not a fall-through to backoff.
    const seconds = Number(retryAfter);
    if (Number.isFinite(seconds) && seconds >= 0) return Math.min(seconds * 1000, 60_000);
    const date = Date.parse(retryAfter);
    if (Number.isFinite(date)) {
      return Math.min(Math.max(date - Date.now(), 0), 60_000);
    }
  }
  const base = 500 * 2 ** Math.min(attempt, 4);
  return Math.min(base + Math.random() * base, 8000);
}

/** Sleep that wakes early when the signal aborts (caller re-checks it). */
function sleep(ms: number, signal?: AbortSignal | null): Promise<void> {
  return new Promise((resolve) => {
    if (signal?.aborted) return resolve();
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      resolve();
    };
    signal?.addEventListener('abort', onAbort, { once: true });
  });
}
