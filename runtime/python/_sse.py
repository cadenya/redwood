# Server-sent events over httpx. Parses the wire format incrementally —
# multi-line data, event names, comments, CRLF. Each event's `data` is
# JSON-decoded and passed through the stream's decoder.
from __future__ import annotations

import json
import threading
import time
from dataclasses import dataclass
from typing import Any, Callable, Generic, Iterator, Optional, Sequence, TypeVar

import httpx

T = TypeVar("T")


@dataclass
class ServerSentEvent(Generic[T]):
    event: Optional[str]
    data: T
    id: Optional[str]


class Stream(Generic[T]):
    """Iterate decoded event payloads; `events()` yields full events."""

    def __init__(
        self,
        response: httpx.Response,
        decoder: Callable[[Any], T],
        last_event_id: Optional[str] = None,
        skip_events: Sequence[str] = (),
        reconnect: Optional[Callable[[Optional[str]], httpx.Response]] = None,
    ) -> None:
        # Auto-reconnect (EventSource semantics): a MID-STREAM transport
        # drop re-issues the request from the resume checkpoint. Clean EOF,
        # close(), and budget exhaustion never reconnect.
        self._reconnect = reconnect
        self._retry_hint_ms: Optional[int] = None
        self._reconnect_attempts = 0
        self._response = response
        self._decoder = decoder
        # Housekeeping event names (the ``event:`` field) skipped without
        # decoding; their ``id:`` fields still advance the checkpoint.
        self._skip_events = frozenset(skip_events)
        self._lifecycle = threading.Lock()
        self._consumed = False
        self._closed = False
        #: The resume checkpoint: seeded from the id this stream was resumed
        #: with, then updated by ``id:`` fields (persistent per the SSE
        #: spec). Pass it as ``last_event_id`` to resume after a disconnect.
        self.last_event_id: Optional[str] = last_event_id

    def __iter__(self) -> Iterator[T]:
        for event in self.events():
            yield event.data

    def events(self) -> Iterator[ServerSentEvent[T]]:
        # A Stream wraps ONE HTTP response (plus transparent auto-reconnect
        # continuations). Re-enumeration must fail with a stable SDK error,
        # and a stream closed before iteration yields nothing.
        with self._lifecycle:
            if self._closed and not self._consumed:
                return
            if self._consumed:
                raise IOError(
                    "stream already consumed - reconnect with a new stream "
                    f"using last_event_id={self.last_event_id!r}"
                )
            self._consumed = True
        data_lines: list[str] = []
        event_name: Optional[str] = None
        first_line = True
        self._reconnect_attempts = 0
        try:
            while True:
                try:
                    for raw_line in self._iter_lines_mapped():
                        # Bytes flowing: the reconnect budget is per-outage.
                        self._reconnect_attempts = 0
                        line = raw_line.rstrip("\r")
                        if first_line:
                            # Event-stream decoding strips at most ONE
                            # leading BOM; without this the first field
                            # parses as BOM+"data" and is discarded.
                            line = line.removeprefix("\ufeff")
                            first_line = False
                        if line == "":
                            if data_lines and event_name not in self._skip_events:
                                payload = json.loads("\n".join(data_lines))
                                # The last-event-ID buffer persists across
                                # events until another `id:` changes it.
                                yield ServerSentEvent(event_name, self._decoder(payload), self.last_event_id)
                            data_lines = []
                            event_name = None
                            continue
                        if line.startswith(":"):
                            continue  # comment / keep-alive
                        field, _, value = line.partition(":")
                        if value.startswith(" "):
                            value = value[1:]
                        if field == "data":
                            data_lines.append(value)
                        elif field == "event":
                            event_name = value
                        elif field == "id":
                            # Ids containing U+0000 are ignored; an empty id
                            # resets the buffer.
                            if "\x00" not in value:
                                self.last_event_id = value or None
                        elif field == "retry":
                            # Delay hint, honored during auto-reconnect.
                            if value.isdigit():
                                self._retry_hint_ms = min(int(value), 60_000)
                except Exception as exc:
                    from ._core import APIConnectionError

                    # Only a MID-STREAM transport drop reconnects; anything
                    # else (protocol errors, decode failures) propagates.
                    if not isinstance(exc, APIConnectionError):
                        raise
                    if not self._try_reconnect():
                        if self._closed:
                            return
                        raise
                    data_lines = []
                    event_name = None
                    first_line = True
                    continue
                break  # clean EOF: API streams may legitimately end
            if data_lines and event_name not in self._skip_events:
                payload = json.loads("\n".join(data_lines))
                yield ServerSentEvent(event_name, self._decoder(payload), self.last_event_id)
        finally:
            self._response.close()

    # Bounded reconnect: 5 consecutive attempts per outage, backoff
    # 500ms*2^n capped 10s (the server's `retry:` hint overrides), sliced
    # sleeps so close() from another thread still returns promptly.
    _MAX_RECONNECTS = 5

    def _try_reconnect(self) -> bool:
        from ._core import APIConnectionError

        while (
            self._reconnect is not None
            and not self._closed
            and self._reconnect_attempts < self._MAX_RECONNECTS
        ):
            delay = (self._retry_hint_ms / 1000.0) if self._retry_hint_ms is not None else min(0.5 * (2 ** self._reconnect_attempts), 10.0)
            self._reconnect_attempts += 1
            waited = 0.0
            while waited < delay:
                if self._closed:
                    return False
                step = min(0.1, delay - waited)
                time.sleep(step)
                waited += step
            if self._closed:
                return False
            try:
                next_response = self._reconnect(self.last_event_id)
            except APIConnectionError:
                # A transport handshake failure (server restarting) consumes
                # budget and retries; HTTP-level failures propagate — a
                # reconnect must not mask expired credentials.
                continue
            if self._closed:
                next_response.close()
                return False
            self._response.close()
            self._response = next_response
            return True
        return False

    def _iter_lines_mapped(self) -> Iterator[str]:
        # Transport failures mid-stream surface as the SDK's connection
        # error (original exception preserved as __cause__), never as raw
        # httpx internals.
        from ._core import APIConnectionError

        try:
            yield from self._response.iter_lines()
        except httpx.HTTPError as exc:
            # A read failure AFTER a deliberate SDK close is the close
            # itself unblocking the transport — end iteration cleanly.
            # Only pre-close failures are genuine connection outages.
            with self._lifecycle:
                if self._closed:
                    return
            raise APIConnectionError(f"stream read failed: {exc}") from exc

    def close(self) -> None:
        with self._lifecycle:
            self._closed = True
        self._response.close()

    def __enter__(self) -> "Stream[T]":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()
