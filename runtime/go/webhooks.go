// Vendored runtime: Standard Webhooks verification
// (https://www.standardwebhooks.com): HMAC-SHA256 over
// "{id}.{timestamp}.{payload}" with the base64-decoded whsec_ secret;
// webhook-signature may carry several space-separated "v1,<base64>" entries.

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"time"
)

// ErrWebhookVerification is returned (wrapped) for any signature failure.
var ErrWebhookVerification = errors.New("webhook verification failed")

const webhookTolerance = 5 * time.Minute

func verifyWebhook(secret string, payload []byte, headers http.Header) error {
	encoded := strings.TrimPrefix(secret, "whsec_")
	key, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil {
		return fmt.Errorf("%w: secret is not valid base64", ErrWebhookVerification)
	}
	// Standard Webhooks symmetric keys are 24–64 bytes. A shorter key —
	// especially the zero-byte key from a bare "whsec_" — must never verify.
	if len(key) < 24 || len(key) > 64 {
		return fmt.Errorf("%w: decoded secret must be 24-64 bytes, got %d", ErrWebhookVerification, len(key))
	}

	id := headers.Get("webhook-id")
	timestamp := headers.Get("webhook-timestamp")
	signatures := headers.Get("webhook-signature")
	if id == "" || timestamp == "" || signatures == "" {
		return fmt.Errorf("%w: missing webhook-id, webhook-timestamp, or webhook-signature header", ErrWebhookVerification)
	}

	// One wire grammar across every SDK: plain non-negative decimal digits
	// (ParseInt alone would also accept a leading '+' or '-').
	for _, r := range timestamp {
		if r < '0' || r > '9' {
			return fmt.Errorf("%w: bad webhook-timestamp", ErrWebhookVerification)
		}
	}
	sent, err := strconv.ParseInt(timestamp, 10, 64)
	if err != nil {
		return fmt.Errorf("%w: bad webhook-timestamp", ErrWebhookVerification)
	}
	drift := time.Since(time.Unix(sent, 0))
	if drift < 0 {
		drift = -drift
	}
	if drift > webhookTolerance {
		return fmt.Errorf("%w: webhook-timestamp outside tolerance", ErrWebhookVerification)
	}

	mac := hmac.New(sha256.New, key)
	fmt.Fprintf(mac, "%s.%s.%s", id, timestamp, payload)
	expected := mac.Sum(nil)

	for _, candidate := range strings.Split(signatures, " ") {
		version, sig, ok := strings.Cut(candidate, ",")
		if !ok || version != "v1" {
			continue
		}
		provided, err := base64.StdEncoding.DecodeString(sig)
		if err != nil {
			continue
		}
		if hmac.Equal(provided, expected) {
			return nil
		}
	}
	return fmt.Errorf("%w: no matching v1 signature", ErrWebhookVerification)
}
