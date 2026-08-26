// Vendored runtime: HTTP core. Auth, query serialization, retries with
// backoff and Retry-After, and google.rpc.Status error mapping — stdlib only.

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"math/rand"
	"net/http"
	"net/url"
	"sort"
	"strconv"
	"strings"
	"time"
)

// debugTransport logs each HTTP exchange to w — request line, headers with
// the credential redacted, and both bodies — without changing behavior.
// Streaming (SSE) response bodies are not consumed: the stream itself is
// the output, so only its status and headers are logged.
type debugTransport struct {
	base http.RoundTripper
	w    io.Writer
}

func (d *debugTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	fmt.Fprintf(d.w, "> %s %s\n", req.Method, req.URL)
	d.dumpHeaders(">", req.Header)
	if req.Body != nil && req.GetBody != nil {
		if body, err := req.GetBody(); err == nil {
			data, _ := io.ReadAll(body)
			body.Close()
			if len(data) > 0 {
				fmt.Fprintf(d.w, "> %s\n", data)
			}
		}
	}
	base := d.base
	if base == nil {
		base = http.DefaultTransport
	}
	resp, err := base.RoundTrip(req)
	if err != nil {
		fmt.Fprintf(d.w, "< transport error: %v\n", err)
		return resp, err
	}
	fmt.Fprintf(d.w, "< HTTP %s\n", resp.Status)
	d.dumpHeaders("<", resp.Header)
	if strings.HasPrefix(resp.Header.Get("Content-Type"), "text/event-stream") {
		fmt.Fprintf(d.w, "< (streaming body not shown)\n")
		return resp, nil
	}
	data, readErr := io.ReadAll(resp.Body)
	resp.Body.Close()
	if len(data) > 0 {
		fmt.Fprintf(d.w, "< %s\n", data)
	}
	resp.Body = io.NopCloser(bytes.NewReader(data))
	if readErr != nil {
		return resp, readErr
	}
	return resp, nil
}

func (d *debugTransport) dumpHeaders(prefix string, h http.Header) {
	keys := make([]string, 0, len(h))
	for k := range h {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		// Redacted, not omitted: the dump must show the header WAS sent
		// without ever writing the credential to a log.
		if k == "Authorization" || strings.EqualFold(k, "X-Api-Key") {
			fmt.Fprintf(d.w, "%s %s: [redacted]\n", prefix, k)
			continue
		}
		for _, v := range h[k] {
			fmt.Fprintf(d.w, "%s %s: %s\n", prefix, k, v)
		}
	}
}

// APIError is a non-2xx response carrying the API's rpc Status payload.
type APIError struct {
	StatusCode int
	Code       int
	Message    string
	Details    []map[string]any
}

func (e *APIError) Error() string {
	if e.Message != "" {
		return fmt.Sprintf("cadenya: %s (http %d, code %d)", e.Message, e.StatusCode, e.Code)
	}
	return fmt.Sprintf("cadenya: http %d", e.StatusCode)
}

// RequestOption customizes a single request.
type RequestOption func(*requestConfig)

type requestConfig struct {
	headers   http.Header
	retries   *int
	reconnect *bool
}

// WithRequestHeader adds a header to one request. Repeating the same key
// appends additional values rather than replacing earlier ones.
func WithRequestHeader(key, value string) RequestOption {
	return func(rc *requestConfig) { rc.headers.Add(key, value) }
}

// WithRequestRetries overrides the retry count for one request. This is the
// explicit opt-in for retrying a non-idempotent method (POST/PATCH).
func WithRequestRetries(n int) RequestOption {
	return func(rc *requestConfig) { rc.retries = &n }
}

// WithReconnect toggles SSE auto-reconnect for one stream request (default
// on): a mid-stream transport drop resumes from the last received event id.
// Pass false to surface drops as errors instead.
func WithReconnect(enabled bool) RequestOption {
	return func(rc *requestConfig) { rc.reconnect = &enabled }
}

// WithLastEventID resumes a server-sent event stream: the id of the last
// event already processed is sent as the Last-Event-ID request header.
func WithLastEventID(id string) RequestOption {
	return func(rc *requestConfig) { rc.headers.Set("Last-Event-ID", id) }
}


type core struct {
	baseURL    string
	authHeader func() (string, string)
	httpClient *http.Client
	maxRetries int
	defaults   map[string]string
	userAgent  string
}

var retryableStatus = map[int]bool{408: true, 409: true, 429: true, 500: true, 502: true, 503: true, 504: true}

// resolveDefault returns the per-call value when set, else the client-level
// default, else an error naming every way to supply it. Presence and
// validity are separate: an EXPLICITLY supplied blank is a configuration
// error and never falls back to ambient client/environment state.
func (c *core) resolveDefault(name, envVar string, perCall *string) (string, error) {
	if perCall != nil {
		v := strings.TrimSpace(*perCall)
		if v == "" {
			return "", fmt.Errorf("cadenya: %q must not be blank", name)
		}
		return v, nil
	}
	if v, ok := c.defaults[name]; ok && v != "" {
		return v, nil
	}
	return "", fmt.Errorf("cadenya: missing %q: pass it in params, set it on the client, or set the %s environment variable", name, envVar)
}

// defaultRequestTimeout bounds ordinary JSON calls when the caller sets no
// deadline. It deliberately does NOT apply to streaming responses — an SSE
// body legitimately outlives any fixed request timeout — which is also why
// the default http.Client carries no whole-request Timeout.
const defaultRequestTimeout = 60 * time.Second

func (c *core) do(ctx context.Context, method, path string, query url.Values, body any, out any, opts ...RequestOption) error {
	if _, hasDeadline := ctx.Deadline(); !hasDeadline {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, defaultRequestTimeout)
		defer cancel()
	}
	resp, err := c.raw(ctx, method, path, query, body, false, opts...)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	// Branch on the GENERATED expectation, not the HTTP status: a void
	// method accepts 204/empty, but an object-returning method requires a
	// representation — a 204 there would fabricate a zero-value resource.
	if out == nil {
		io.Copy(io.Discard, resp.Body)
		return nil
	}
	if resp.StatusCode == http.StatusNoContent {
		return fmt.Errorf("cadenya: protocol error: HTTP 204 where a JSON response was expected")
	}
	data, err := io.ReadAll(resp.Body)
	if err != nil {
		return fmt.Errorf("cadenya: reading response: %w", err)
	}
	// A method with an output must receive a JSON document: an empty body or
	// a top-level null would otherwise fabricate a zero-value "resource" that
	// looks decoded until a later nil dereference. (Void methods and 204
	// returned above.)
	trimmed := bytes.TrimSpace(data)
	if len(trimmed) == 0 {
		return fmt.Errorf("cadenya: protocol error: HTTP %d with an empty body where a JSON response was expected", resp.StatusCode)
	}
	if bytes.Equal(trimmed, []byte("null")) {
		return fmt.Errorf("cadenya: protocol error: HTTP %d with a JSON null body where a JSON response was expected", resp.StatusCode)
	}
	if err := json.Unmarshal(data, out); err != nil {
		return fmt.Errorf("cadenya: decoding response: %w", err)
	}
	return nil
}

// buildRequestConfig applies every option exactly once. Options are public
// callbacks and may be stateful, so they must never be replayed to
// reconstruct request state after the fact.
func buildRequestConfig(opts []RequestOption) *requestConfig {
	rc := &requestConfig{headers: http.Header{}}
	for _, opt := range opts {
		opt(rc)
	}
	return rc
}

func (c *core) raw(ctx context.Context, method, path string, query url.Values, body any, stream bool, opts ...RequestOption) (*http.Response, error) {
	return c.rawConfig(ctx, method, path, query, body, stream, buildRequestConfig(opts))
}

func (c *core) rawConfig(ctx context.Context, method, path string, query url.Values, body any, stream bool, rc *requestConfig) (*http.Response, error) {

	u := strings.TrimRight(c.baseURL, "/") + path
	if len(query) > 0 {
		u += "?" + query.Encode()
	}

	var payload []byte
	if body != nil {
		var err error
		payload, err = json.Marshal(body)
		if err != nil {
			return nil, fmt.Errorf("cadenya: encoding request body: %w", err)
		}
	}

	// Automatic retries apply only to idempotent methods: a POST/PATCH that
	// succeeds server-side but loses its response would run twice. Callers
	// opt a specific mutation in with WithRequestRetries.
	maxRetries := c.maxRetries
	switch method {
	case "GET", "HEAD", "PUT", "DELETE":
	default:
		maxRetries = 0
	}
	if rc.retries != nil {
		maxRetries = *rc.retries
	}
	if maxRetries < 0 {
		maxRetries = 0
	}

	for attempt := 0; ; attempt++ {
		var reader io.Reader
		if payload != nil {
			reader = bytes.NewReader(payload)
		}
		req, err := http.NewRequestWithContext(ctx, method, u, reader)
		if err != nil {
			return nil, err
		}
		if payload != nil {
			req.Header.Set("Content-Type", "application/json")
		}
		if stream {
			req.Header.Set("Accept", "text/event-stream")
		} else {
			req.Header.Set("Accept", "application/json")
		}
		req.Header.Set("User-Agent", c.userAgent)
		if key, value := c.authHeader(); key != "" {
			req.Header.Set(key, value)
		}
		for k, vs := range rc.headers {
			// Replace any default for this key, then keep every caller value.
			req.Header.Del(k)
			for _, v := range vs {
				req.Header.Add(k, v)
			}
		}

		resp, err := c.httpClient.Do(req)
		if err != nil {
			if ctx.Err() != nil {
				return nil, ctx.Err()
			}
			if attempt < maxRetries {
				if sleepErr := sleepBackoff(ctx, attempt, ""); sleepErr != nil {
					return nil, sleepErr
				}
				continue
			}
			return nil, fmt.Errorf("cadenya: connection error: %w", err)
		}

		if resp.StatusCode >= 200 && resp.StatusCode < 300 {
			return resp, nil
		}

		if retryableStatus[resp.StatusCode] && attempt < maxRetries {
			retryAfter := resp.Header.Get("Retry-After")
			io.Copy(io.Discard, resp.Body)
			resp.Body.Close()
			if sleepErr := sleepBackoff(ctx, attempt, retryAfter); sleepErr != nil {
				return nil, sleepErr
			}
			continue
		}

		apiErr := &APIError{StatusCode: resp.StatusCode}
		var status struct {
			Code    int              `json:"code"`
			Message string           `json:"message"`
			Details []map[string]any `json:"details"`
		}
		data, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		if json.Unmarshal(data, &status) == nil {
			apiErr.Code = status.Code
			apiErr.Message = status.Message
			apiErr.Details = status.Details
		}
		return nil, apiErr
	}
}

// sleepBackoff waits before the next attempt, honoring a Retry-After header
// (delta-seconds or HTTP-date; `Retry-After: 0` means retry immediately).
// Returns ctx.Err() when cancelled so callers stop instead of issuing one
// more doomed attempt.
func sleepBackoff(ctx context.Context, attempt int, retryAfter string) error {
	delay := time.Duration(-1)
	if retryAfter != "" {
		if secs, err := strconv.Atoi(retryAfter); err == nil && secs >= 0 {
			delay = time.Duration(secs) * time.Second
		} else if at, err := http.ParseTime(retryAfter); err == nil {
			delay = time.Until(at)
		}
		if delay > time.Minute {
			delay = time.Minute
		}
	}
	if delay < 0 {
		// Cap the shift so a huge retry count cannot overflow.
		shift := attempt
		if shift > 4 {
			shift = 4
		}
		base := 500 * time.Millisecond * (1 << shift)
		delay = base + time.Duration(rand.Int63n(int64(base)))
		if delay > 8*time.Second {
			delay = 8 * time.Second
		}
	}
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-time.After(delay):
		return nil
	}
}

// pathSegment validates and escapes one URL path segment: a blank identifier
// would silently rewrite the route (/parents//children), so it fails before
// any network I/O. The original (untrimmed) value is escaped when non-blank.
func pathSegment(name, value string) (string, error) {
	if strings.TrimSpace(value) == "" {
		return "", fmt.Errorf("cadenya: missing required path parameter %q", name)
	}
	return url.PathEscape(value), nil
}

// Ptr returns a pointer to v; handy for optional params.
func Ptr[T any](v T) *T { return &v }

// String returns a pointer to s (go-github style helper).
func String(s string) *string { return &s }

// Int32 returns a pointer to i.
func Int32(i int32) *int32 { return &i }

// Int64 returns a pointer to i.
func Int64(i int64) *int64 { return &i }

// Bool returns a pointer to b.
func Bool(b bool) *bool { return &b }

// Float64 returns a pointer to f.
func Float64(f float64) *float64 { return &f }
