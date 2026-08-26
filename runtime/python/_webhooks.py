# Standard Webhooks verification (https://www.standardwebhooks.com).
# HMAC-SHA256 over `{id}.{timestamp}.{payload}` keyed by the base64-decoded
# secret (`whsec_` prefix optional); `webhook-signature` may carry several
# space-separated `v1,<base64>` candidates.
from __future__ import annotations

import base64
import hashlib
import hmac
import math
import re
import time
from typing import Mapping


class WebhookVerificationError(Exception):
    pass


def verify_webhook(
    secret: str,
    payload: bytes,
    headers: Mapping[str, str],
    *,
    tolerance_seconds: int = 300,
) -> None:
    """Raises WebhookVerificationError unless the payload is authentic."""
    normalized = {k.lower(): v for k, v in headers.items()}
    msg_id = normalized.get("webhook-id")
    timestamp = normalized.get("webhook-timestamp")
    signatures = normalized.get("webhook-signature")
    if not msg_id or not timestamp or not signatures:
        raise WebhookVerificationError(
            "missing webhook-id, webhook-timestamp, or webhook-signature header"
        )

    if not isinstance(secret, str):
        raise WebhookVerificationError("webhook secret must be a string")

    # Tolerance must be a finite non-negative number — and not a bool, which
    # would silently pass the int check.
    if (
        isinstance(tolerance_seconds, bool)
        or not isinstance(tolerance_seconds, (int, float))
        or not math.isfinite(tolerance_seconds)
        or tolerance_seconds < 0
    ):
        raise WebhookVerificationError("tolerance_seconds must be a finite non-negative number")

    # The spec calls webhook-timestamp an integer Unix timestamp. int()'s
    # broader grammar ("+1", " 1 ", "1_0") would disagree across SDKs, and a
    # float grammar would accept "nan" (comparisons always false — a replay-
    # window bypass). Bound it so float arithmetic can't overflow either.
    if not re.fullmatch(r"[0-9]+", str(timestamp)):
        raise WebhookVerificationError("webhook-timestamp is not an integer")
    sent = int(timestamp)
    if sent > 2**63:
        raise WebhookVerificationError("webhook-timestamp is out of range")
    if abs(time.time() - sent) > tolerance_seconds:
        raise WebhookVerificationError("webhook-timestamp outside tolerance")

    encoded = secret.removeprefix("whsec_")
    try:
        key = base64.b64decode(encoded, validate=True)
    except Exception:
        raise WebhookVerificationError("webhook secret is not valid base64") from None
    # Standard Webhooks symmetric keys are 24–64 bytes. A shorter key —
    # especially the zero-byte key from a bare "whsec_" — must never verify.
    if not 24 <= len(key) <= 64:
        raise WebhookVerificationError(f"decoded webhook secret must be 24-64 bytes, got {len(key)}")
    data = f"{msg_id}.{timestamp}.".encode() + payload
    expected = hmac.new(key, data, hashlib.sha256).digest()

    for candidate in signatures.split(" "):
        version, _, signature = candidate.partition(",")
        if version != "v1" or not signature:
            continue
        try:
            provided = base64.b64decode(signature)
        except Exception:
            continue
        if hmac.compare_digest(provided, expected):
            return
    raise WebhookVerificationError("no matching v1 signature")
