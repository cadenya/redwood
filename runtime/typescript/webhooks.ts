/**
 * Standard Webhooks verification (https://www.standardwebhooks.com), built on
 * WebCrypto — no dependencies. The signature is HMAC-SHA256 over
 * `${id}.${timestamp}.${payload}` keyed by the base64-decoded secret
 * (`whsec_` prefix optional); `webhook-signature` may carry several
 * space-separated `v1,<base64>` candidates.
 */

export class WebhookVerificationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'WebhookVerificationError';
  }
}

export interface WebhookVerifyOptions {
  /** Max allowed clock skew for webhook-timestamp, in seconds. Default 300. */
  toleranceSeconds?: number;
}

/** Plain header records and Fetch-style Headers objects are both accepted. */
export type WebhookHeaders =
  | Record<string, string | string[] | undefined>
  | { get(name: string): string | null };

export class WebhookVerifier {
  private readonly key: Promise<CryptoKey>;

  constructor(secret: string) {
    if (!secret?.trim()) {
      throw new WebhookVerificationError('webhook secret must be a non-empty string');
    }
    const encoded = secret.startsWith('whsec_') ? secret.slice('whsec_'.length) : secret;
    // atob is forgiving (whitespace, missing padding); require the canonical
    // form so every SDK accepts exactly the same secrets.
    if (encoded.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(encoded)) {
      throw new WebhookVerificationError('webhook secret is not valid base64');
    }
    const raw = (() => {
      try {
        return base64ToBytes(encoded);
      } catch {
        throw new WebhookVerificationError('webhook secret is not valid base64');
      }
    })();
    // Standard Webhooks symmetric keys are 24–64 bytes. A shorter key —
    // especially the zero-byte key from a bare "whsec_" — must never verify.
    if (raw.length < 24 || raw.length > 64) {
      throw new WebhookVerificationError(
        `decoded webhook secret must be 24-64 bytes, got ${raw.length}`,
      );
    }
    this.key = crypto.subtle
      .importKey('raw', raw, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign'])
      .catch(() => {
        throw new WebhookVerificationError('webhook secret could not be imported as an HMAC key');
      });
  }

  /** Throws WebhookVerificationError unless the payload is authentic. */
  async verify(payload: string, headers: WebhookHeaders, options: WebhookVerifyOptions = {}): Promise<void> {
    const header = normalizeHeaders(headers);
    const id = header['webhook-id'];
    const timestamp = header['webhook-timestamp'];
    const signatures = header['webhook-signature'];
    if (!id || !timestamp || !signatures) {
      throw new WebhookVerificationError(
        'missing webhook-id, webhook-timestamp, or webhook-signature header',
      );
    }

    const tolerance = options.toleranceSeconds ?? 300;
    if (!Number.isFinite(tolerance) || tolerance < 0) {
      // An infinite tolerance would disable the replay window entirely.
      throw new WebhookVerificationError('toleranceSeconds must be a finite non-negative number');
    }
    // The spec calls webhook-timestamp an integer Unix timestamp; Number()'s
    // broader grammar would disagree with the other SDKs on the same input.
    if (!/^[0-9]+$/.test(timestamp)) {
      throw new WebhookVerificationError('webhook-timestamp is not an integer');
    }
    const sent = Number(timestamp);
    if (!Number.isSafeInteger(sent)) {
      throw new WebhookVerificationError('webhook-timestamp is out of range');
    }
    const now = Math.floor(Date.now() / 1000);
    if (Math.abs(now - sent) > tolerance) {
      throw new WebhookVerificationError('webhook-timestamp outside tolerance');
    }

    const key = await this.key;
    const data = new TextEncoder().encode(`${id}.${timestamp}.${payload}`);
    const expected = new Uint8Array(await crypto.subtle.sign('HMAC', key, data));

    for (const candidate of signatures.split(' ')) {
      const [version, signature] = candidate.split(',', 2);
      if (version !== 'v1' || !signature) continue;
      const provided = tryBase64ToBytes(signature);
      if (provided && constantTimeEqual(provided, expected)) return;
    }
    throw new WebhookVerificationError('no matching v1 signature');
  }
}

function normalizeHeaders(headers: WebhookHeaders): Record<string, string | undefined> {
  const out: Record<string, string | undefined> = {};
  if (typeof (headers as { get?: unknown }).get === 'function') {
    // Fetch-style Headers: Object.entries would not see its values.
    const getter = headers as { get(name: string): string | null };
    for (const name of ['webhook-id', 'webhook-timestamp', 'webhook-signature']) {
      out[name] = getter.get(name) ?? undefined;
    }
    return out;
  }
  for (const [key, value] of Object.entries(headers)) {
    out[key.toLowerCase()] = Array.isArray(value) ? value[0] : value;
  }
  return out;
}

function base64ToBytes(b64: string) {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function tryBase64ToBytes(b64: string) {
  try {
    return base64ToBytes(b64);
  } catch {
    return undefined;
  }
}

function constantTimeEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}
