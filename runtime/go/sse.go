// Vendored runtime: server-sent events. The wire protocol is parsed by the
// maintained github.com/tmaxmax/go-sse package (browser-compliant
// last-event-ID persistence, NUL-id handling, CR/LF/CRLF framing, bounded
// event size); this file is only the typed pull-style adapter around it.
// Scanner-style iteration: for stream.Next() { use stream.Current() };
// err := stream.Err(); defer stream.Close().

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"
	"sync"
	"time"

	sse "github.com/tmaxmax/go-sse"
)

// maxEventSize matches the previous hand-rolled parser's ceiling.
const maxEventSize = 8 << 20

type streamItem struct {
	event sse.Event
	err   error
}

// Stream iterates decoded SSE payloads of type T.
type Stream[T any] struct {
	// The request context: once it is done the stream is over — no
	// reconnect can succeed against it, and callers (a CLI's Ctrl-C) expect
	// Next to return promptly rather than after the whole backoff budget.
	ctx         context.Context
	resp        *http.Response
	items       chan streamItem
	cancel      chan struct{}
	closeOnce   sync.Once
	closeErr    error
	current     T
	lastEventID string
	err         error
	done        bool
	// Housekeeping event names (the `event:` field) skipped without
	// decoding; their ids still advance the resume checkpoint.
	skipEvents []string
	// Auto-reconnect (EventSource semantics): re-issues the request from
	// the resume checkpoint on a MID-STREAM transport drop. Clean EOF,
	// Close, and budget exhaustion never reconnect. nil disables.
	reconnect         func(lastEventID string) (*http.Response, error)
	reconnectAttempts int
}

const maxReconnects = 5

// newStream adapts the response body. lastEventID seeds the resume
// checkpoint: per SSE semantics a stream resumed from an id KEEPS that id
// until the server sends a new one — and an explicit empty `id:` CLEARS it.
// The parser must own that one state machine, so the seed is injected as a
// synthetic `id:` line ahead of the body rather than tracked separately
// (separate tracking cannot distinguish "cleared" from "absent").
func newStream[T any](ctx context.Context, resp *http.Response, lastEventID string, skipEvents []string, reconnect func(string) (*http.Response, error)) *Stream[T] {
	if ctx == nil {
		ctx = context.Background()
	}
	s := &Stream[T]{
		ctx:         ctx,
		resp:        resp,
		cancel:      make(chan struct{}),
		lastEventID: lastEventID,
		skipEvents:  skipEvents,
		reconnect:   reconnect,
	}
	s.start(resp, lastEventID)
	return s
}

// start spawns the reader goroutine for one connection, seeding the parser's
// last-event-ID state machine (see newStream doc) and replacing the items
// channel — each connection owns exactly one channel.
func (s *Stream[T]) start(resp *http.Response, seedID string) {
	items := make(chan streamItem)
	s.items = items
	s.resp = resp
	var body io.Reader = resp.Body
	if seedID != "" && !strings.ContainsAny(seedID, "\r\n\x00") {
		body = io.MultiReader(strings.NewReader("id: "+seedID+"\n\n"), resp.Body)
	}
	go func() {
		defer close(items)
		sse.Read(body, &sse.ReadConfig{MaxEventSize: maxEventSize})(func(event sse.Event, err error) bool {
			select {
			case items <- streamItem{event: event, err: err}:
				return err == nil
			case <-s.cancel:
				return false
			}
		})
	}()
}

// tryReconnect swaps in a fresh connection resumed from the checkpoint.
// Transport handshake failures consume budget and retry; HTTP-level
// failures (*APIError — e.g. expired credentials) propagate immediately.
func (s *Stream[T]) tryReconnect() (bool, error) {
	for s.reconnect != nil && !s.canceled() && s.ctx.Err() == nil && s.reconnectAttempts < maxReconnects {
		delay := 500 * time.Millisecond << s.reconnectAttempts
		if delay > 10*time.Second {
			delay = 10 * time.Second
		}
		s.reconnectAttempts++
		select {
		case <-s.cancel:
			return false, nil
		case <-s.ctx.Done():
			return false, nil
		case <-time.After(delay):
		}
		resp, err := s.reconnect(s.lastEventID)
		if err != nil {
			var apiErr *APIError
			if errors.As(err, &apiErr) {
				return false, err
			}
			continue
		}
		if s.canceled() {
			resp.Body.Close()
			return false, nil
		}
		s.resp.Body.Close()
		s.start(resp, s.lastEventID)
		return true, nil
	}
	return false, nil
}

// Next advances to the next event, reporting false at end of stream or error.
func (s *Stream[T]) Next() bool {
	if s.done {
		return false
	}
	for {
		item, ok := <-s.items
		if !ok {
			s.finish(nil)
			return false
		}
		if item.err == nil {
			// Bytes flowing again: the reconnect budget is per-outage.
			s.reconnectAttempts = 0
		}
		if item.err != nil {
			// A read error observed AFTER an explicit Close is the close
			// itself unblocking the body read — deliberate cancellation must
			// not nondeterministically surface as stream failure. Genuine
			// pre-close errors (context cancellation included) arrive with
			// the cancel channel still open and stay visible.
			if s.canceled() {
				s.finish(nil)
				return false
			}
			// Drain the dead connection's channel, then attempt resume.
			for range s.items {
			}
			// A done request context is the caller ending the stream (Ctrl-C,
			// deadline): report the cancellation itself, immediately — never
			// the transport's wrapped read error after a futile backoff.
			if err := s.ctx.Err(); err != nil {
				s.finish(err)
				return false
			}
			ok, rerr := s.tryReconnect()
			if rerr != nil {
				s.finish(rerr)
				return false
			}
			if ok {
				continue
			}
			s.finish(item.err)
			return false
		}
		// Unconditional: the parser's checkpoint is authoritative, and ""
		// legitimately means "the server cleared it with an empty id:".
		s.lastEventID = item.event.LastEventID
		// DOCUMENTED POLICY (schema-independent): events with empty data
		// are metadata/keep-alives — empty data cannot decode into a typed
		// JSON payload T, and the seed injection above plus bare-id pings
		// legitimately produce such events. go-sse's Event cannot
		// distinguish `data:` present-but-empty from no data field at all,
		// so an empty payload cannot be surfaced as a decode error here.
		if item.event.Data == "" {
			continue
		}
		// Housekeeping frames (e.g. ping/open) never reach the consumer and
		// never JSON-decode - the checkpoint above has already advanced.
		if slicesContains(s.skipEvents, item.event.Type) {
			continue
		}
		var value T
		if err := json.Unmarshal([]byte(item.event.Data), &value); err != nil {
			s.finish(err)
			return false
		}
		s.current = value
		return true
	}
}

func (s *Stream[T]) finish(err error) {
	s.done = true
	if err != nil {
		s.err = err
	}
	s.Close()
}

// Current returns the event decoded by the last successful Next.
func (s *Stream[T]) Current() T { return s.current }

// LastEventID returns the resume checkpoint: the id this stream was resumed
// from until the server sends a newer one. Pass it to the stream method via
// WithLastEventID to resume after a disconnect.
func (s *Stream[T]) LastEventID() string { return s.lastEventID }

// Err returns the terminal error, if any.
func (s *Stream[T]) Err() error { return s.err }

func (s *Stream[T]) canceled() bool {
	select {
	case <-s.cancel:
		return true
	default:
		return false
	}
}

// Close releases the underlying connection. Safe to call more than once:
// the body is closed exactly once and every call returns that first close's
// error (the documented EOF→finish→deferred-Close pattern double-calls it).
func (s *Stream[T]) Close() error {
	s.closeOnce.Do(func() {
		close(s.cancel)
		s.closeErr = s.resp.Body.Close()
	})
	return s.closeErr
}

// slicesContains avoids importing slices into this file's minimal set.
func slicesContains(list []string, value string) bool {
	for _, item := range list {
		if item == value {
			return true
		}
	}
	return false
}
