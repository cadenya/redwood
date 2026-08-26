# The hand-written HTTP core: httpx transport, retries with backoff and
# Retry-After, error mapping to the API's google.rpc Status payload, and
# client-level default params.
from __future__ import annotations

import datetime as _dt
import json as _json
import math as _math
import os
import random
import threading
import time
from typing import Any, Iterator, Mapping
from urllib.parse import quote as _quote, urlparse as _urlparse

import httpx

from dataclasses import dataclass as _dataclass

RETRYABLE_STATUS = {408, 409, 429, 500, 502, 503, 504}

# Presence sentinel: `None` can be a legitimate JSON body (null), so "no
# body" needs its own marker.
_UNSET: Any = object()

# Automatic retries apply only to idempotent methods: a POST/PATCH that
# succeeds server-side but loses its response would be executed twice.
IDEMPOTENT_METHODS = {"GET", "HEAD", "PUT", "DELETE"}


@_dataclass(frozen=True)
class RequestOptions:
    """Per-call transport controls, kept visibly separate from API fields.

    An explicit ``max_retries`` here is the caller opting THIS call into (or
    out of) retries — it applies even to mutations, overriding the
    idempotency default.
    """

    headers: Mapping[str, str] | None = None
    timeout: float | None = None
    max_retries: int | None = None
    #: SSE auto-reconnect (default on). False surfaces mid-stream transport
    #: drops as APIConnectionError instead of resuming.
    reconnect: bool | None = None

    def __post_init__(self) -> None:
        if self.headers is not None:
            if not isinstance(self.headers, Mapping):
                raise TypeError("request_options.headers must be a mapping of str to str")
            for key, value in self.headers.items():
                if not isinstance(key, str) or not isinstance(value, str):
                    raise TypeError("request_options.headers must be a mapping of str to str")
        if self.timeout is not None:
            if isinstance(self.timeout, bool) or not isinstance(self.timeout, (int, float)):
                raise TypeError("request_options.timeout must be a number of seconds")
            if not _math.isfinite(self.timeout) or self.timeout <= 0:
                raise ValueError("request_options.timeout must be a positive finite number")
        if self.max_retries is not None:
            if isinstance(self.max_retries, bool) or not isinstance(self.max_retries, int):
                raise TypeError("request_options.max_retries must be an int")
            if self.max_retries < 0:
                raise ValueError("request_options.max_retries must be >= 0")


class APIError(Exception):
    """A non-2xx response carrying the API's rpc Status payload."""

    def __init__(
        self,
        status_code: int,
        code: int | None = None,
        message: str | None = None,
        details: list[dict[str, Any]] | None = None,
    ) -> None:
        super().__init__(message or f"HTTP {status_code}")
        self.status_code = status_code
        self.code = code
        self.message = message or f"HTTP {status_code}"
        self.details = details


class APIConnectionError(Exception):
    """The request never produced an HTTP response."""


class APIResponseError(Exception):
    """The server answered, but not with the JSON the API contract promises
    (e.g. an unfollowed redirect or a non-JSON success body)."""

    def __init__(self, status_code: int, message: str, body: str | None = None) -> None:
        super().__init__(message)
        self.status_code = status_code
        self.body = body


def _rfc3339(value: _dt.datetime) -> str:
    # A naive datetime has no offset, so its isoformat() is not a valid
    # RFC 3339 date-time. Guessing the caller's zone is dangerous — reject
    # loudly; pass a timezone-aware datetime (or a raw string) instead.
    if value.tzinfo is None or value.tzinfo.utcoffset(value) is None:
        raise ValueError(
            "naive datetime has no UTC offset; pass a timezone-aware datetime "
            "(e.g. datetime.now(timezone.utc)) or a preformatted string"
        )
    return value.isoformat()


def _encode_json(value: Any) -> Any:
    if isinstance(value, _dt.datetime):
        return _rfc3339(value)
    raise TypeError(f"not JSON serializable: {type(value).__name__}")


def _query_value(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, _dt.datetime):
        return _rfc3339(value)
    return str(value)


def decode_response(operation: str, data: Any, decoder: Any) -> Any:
    """Run a generated response decoder inside the SDK's stable error
    boundary: a 2xx body that is valid JSON but structurally wrong for the
    contract raises APIResponseError with the operation context and the
    original failure chained — never a raw AttributeError/KeyError from
    generated decoder internals."""
    try:
        return decoder(data)
    except APIResponseError:
        raise
    except Exception as exc:  # noqa: BLE001 - the decode path is generated code
        raise APIResponseError(
            0,
            f"{operation}: 2xx response does not match the API contract: {exc}",
        ) from exc


def parse_datetime(value: str) -> _dt.datetime:
    return _dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


def path_param(name: str, value: Any) -> str:
    """Encode one path segment, rejecting blank values at the boundary — an
    empty segment silently rewrites the route (/parents//children)."""
    text = str(value) if value is not None else ""
    if not text.strip():
        raise ValueError(f"missing required path parameter {name!r}")
    return _quote(text, safe="")


class Core:
    def __init__(
        self,
        *,
        base_url: str,
        auth_header: tuple[str, str],
        http_client: httpx.Client | None,
        max_retries: int,
        defaults: dict[str, str],
        user_agent: str,
    ) -> None:
        # Validate the STRUCTURE once: operation paths are appended to
        # this value, so a query/fragment/userinfo would silently swallow
        # the request path. Absolute http(s) with a host is required; a
        # path prefix is supported and kept.
        # A literal delimiter parses as an EMPTY query/fragment and slips
        # past truthiness checks — "https://host?" + "/v1/x" still swallows
        # the path into query text. Reject the characters outright.
        if "?" in base_url or "#" in base_url:
            raise ValueError(
                f"base_url {base_url!r} must not carry userinfo, query, or fragment"
            )
        _parsed = _urlparse(base_url)
        if _parsed.scheme not in ("http", "https"):
            raise ValueError(f"base_url {base_url!r} must be an absolute http(s) URL")
        if not _parsed.hostname:
            raise ValueError(f"base_url {base_url!r} has no host")
        if _parsed.username or _parsed.password or _parsed.query or _parsed.fragment:
            raise ValueError(
                f"base_url {base_url!r} must not carry userinfo, query, or fragment"
            )
        self.base_url = base_url.rstrip("/")
        self.auth_header = auth_header
        # Only close transports we created; a caller-supplied client is theirs.
        self._owns_client = http_client is None
        self.http_client = http_client or httpx.Client(timeout=60.0)
        # Finite bounded integer: NaN/inf/fractions/negatives all normalize.
        try:
            retries = int(max_retries)
        except (TypeError, ValueError, OverflowError):
            retries = 0
        self.max_retries = min(max(0, retries), 10)
        self.defaults = defaults
        self.user_agent = user_agent

    def close(self) -> None:
        if self._owns_client:
            self.http_client.close()

    def resolve_default(self, wire_name: str, env_var: str, value: str | None) -> str:
        # Presence and validity are separate: an EXPLICITLY supplied blank is
        # a configuration error and must never fall back to ambient client/
        # environment state (that could silently target another tenant/scope).
        if value is not None:
            trimmed = str(value).strip()
            if not trimmed:
                raise ValueError(f"{wire_name} must not be blank")
            return trimmed
        resolved = (self.defaults.get(wire_name) or os.environ.get(env_var, "")).strip()
        if not resolved:
            raise ValueError(
                f"missing {wire_name}: pass it, set it on the client, or set {env_var}"
            )
        return resolved

    def _build(
        self,
        method: str,
        path: str,
        query: Mapping[str, Any] | None,
        body: Any,
        headers: Mapping[str, str] | None,
        stream: bool,
        options: RequestOptions | None,
    ) -> httpx.Request:
        params: list[tuple[str, str]] = []
        for key, value in (query or {}).items():
            if value is None:
                continue
            if isinstance(value, (list, tuple)):
                params.extend((key, _query_value(item)) for item in value)
            else:
                params.append((key, _query_value(value)))
        request_headers = {
            "User-Agent": self.user_agent,
            "Accept": "text/event-stream" if stream else "application/json",
        }
        if self.auth_header[0]:
            request_headers[self.auth_header[0]] = self.auth_header[1]
        content = None
        if body is not _UNSET:
            # Serialize the body EXACTLY: objects, arrays, scalars, booleans,
            # and explicit null are all legitimate whole bodies. Field
            # omission for flattened object bodies happens in the generated
            # resource methods, never here.
            request_headers["Content-Type"] = "application/json"
            try:
                content = _json.dumps(body, default=_encode_json)
            except (TypeError, ValueError) as exc:
                raise ValueError(f"request body is not JSON-serializable: {exc}") from exc
        # Precedence: generated defaults < auth < per-request option headers <
        # semantic headers (Last-Event-ID resume state stays authoritative).
        # Replacement is case-insensitive as HTTP requires.
        if options is not None and options.headers:
            for key, value in options.headers.items():
                for existing in [k for k in request_headers if k.lower() == key.lower()]:
                    del request_headers[existing]
                request_headers[key] = value
        for key, value in (headers or {}).items():
            for existing in [k for k in request_headers if k.lower() == key.lower()]:
                del request_headers[existing]
            request_headers[key] = value
        request = self.http_client.build_request(
            method,
            self.base_url + path,
            params=params or None,
            content=content,
            headers=request_headers,
        )
        if options is not None and options.timeout is not None:
            t = float(options.timeout)
            request.extensions["timeout"] = {
                "connect": t, "read": t, "write": t, "pool": t,
            }
        if stream:
            # A quiet-but-healthy SSE stream must not die on the ordinary
            # read timeout. Only the read phase changes: the client's own
            # connect/write/pool policy (caller-supplied or default) is
            # preserved by copying the mapping httpx already attached.
            timeout = dict(request.extensions.get("timeout", {}))
            timeout["read"] = None
            request.extensions["timeout"] = timeout
        return request

    def _send(
        self,
        request: httpx.Request,
        stream: bool,
        options: RequestOptions | None = None,
    ) -> httpx.Response:
        if options is not None and options.max_retries is not None:
            # An explicit per-request value is the caller opting this exact
            # call in/out — it overrides the idempotency default, mutations
            # included. The body is fixed bytes, so replay is safe.
            max_retries = min(options.max_retries, 10)
        else:
            max_retries = self.max_retries if request.method in IDEMPOTENT_METHODS else 0
        last_exc: Exception | None = None
        for attempt in range(max_retries + 1):
            try:
                response = self.http_client.send(request, stream=stream)
            except httpx.HTTPError as exc:
                last_exc = exc
                if attempt >= max_retries:
                    raise APIConnectionError(str(exc)) from exc
                time.sleep(_backoff_seconds(attempt, None))
                continue
            if response.status_code in RETRYABLE_STATUS and attempt < max_retries:
                retry_after = response.headers.get("retry-after")
                response.close()
                time.sleep(_backoff_seconds(attempt, retry_after))
                continue
            return response
        raise APIConnectionError(str(last_exc))  # pragma: no cover

    # A non-2xx streamed body is read only up to this bound: the stream's
    # ordinary read timeout was disabled before its status was known, so an
    # endless/huge error body must not hang or exhaust memory.
    _MAX_ERROR_BODY = 65536
    # Deadline for that same diagnostic read (seconds).
    _MAX_ERROR_BODY_SECONDS = 10.0

    @classmethod
    def _raise_for_status(cls, response: httpx.Response, streamed: bool) -> None:
        if 200 <= response.status_code < 300:
            return
        if streamed:
            # Bounded in BYTES and TIME, failure-mapped, and ALWAYS closed.
            # The successful-stream read timeout was disabled before the
            # status was known, so a server that sends failure headers and
            # then stalls would otherwise hang this read forever. The read
            # runs in a helper thread; on deadline the response is closed,
            # which unblocks the socket read.
            chunks: list[bytes] = []
            failure: list[Exception] = []

            def _read_error_body() -> None:
                try:
                    total = 0
                    for chunk in response.iter_bytes():
                        chunks.append(chunk)
                        total += len(chunk)
                        if total >= cls._MAX_ERROR_BODY:
                            break
                except Exception as exc:  # noqa: BLE001 — mapped below
                    failure.append(exc)

            reader = threading.Thread(target=_read_error_body, daemon=True)
            reader.start()
            reader.join(cls._MAX_ERROR_BODY_SECONDS)
            timed_out = reader.is_alive()
            response.close()
            reader.join(1.0)
            body_text = b"".join(chunks)[: cls._MAX_ERROR_BODY].decode("utf-8", errors="replace")
            # A transport failure with NO captured diagnostic is a connection
            # error; a timeout or partial read still reports the API failure
            # with whatever prefix arrived (the status code is the signal).
            if failure and not chunks and not timed_out:
                exc = failure[0]
                raise APIConnectionError(
                    f"reading error body of HTTP {response.status_code} stream response: {exc}"
                ) from exc
        else:
            body_text = response.text
            response.close()
        if response.status_code < 400:
            # An unfollowed redirect is a protocol surprise, not an API error.
            raise APIResponseError(
                response.status_code,
                f"unexpected non-2xx response (HTTP {response.status_code})",
                body=body_text[:2000],
            )
        try:
            status = _json.loads(body_text)
        except ValueError:
            status = {}
        if not isinstance(status, dict):
            # A proxy can answer 4xx/5xx with `[]`, `null`, or a bare string.
            status = {}
        raise APIError(
            response.status_code,
            code=status.get("code"),
            message=status.get("message"),
            details=status.get("details"),
        )

    def request(
        self,
        method: str,
        path: str,
        *,
        query: Mapping[str, Any] | None = None,
        body: Any = _UNSET,
        headers: Mapping[str, str] | None = None,
        expects_body: bool = True,
        request_options: RequestOptions | None = None,
    ) -> Any:
        if request_options is not None and not isinstance(request_options, RequestOptions):
            raise TypeError("request_options must be a RequestOptions instance")
        response = self._send(
            self._build(method, path, query, body, headers, False, request_options),
            False,
            request_options,
        )
        self._raise_for_status(response, streamed=False)
        # Branch on the GENERATED expectation, not the HTTP status: a void
        # method accepts 204/empty, but an output-bearing method requires a
        # JSON document — empty/null would fabricate a resource (or an empty
        # page) outside the declared contract.
        if not expects_body:
            return None
        text = response.text
        if response.status_code == 204 or not text.strip():
            raise APIResponseError(
                response.status_code,
                f"HTTP {response.status_code} with an empty body where a JSON response was expected",
            )
        try:
            parsed = _json.loads(text)
        except ValueError:
            raise APIResponseError(
                response.status_code,
                "response body is not valid JSON",
                body=text[:2000],
            ) from None
        if parsed is None:
            raise APIResponseError(
                response.status_code,
                f"HTTP {response.status_code} with a JSON null body where a JSON response was expected",
            )
        return parsed

    def raw(
        self,
        method: str,
        path: str,
        *,
        query: Mapping[str, Any] | None = None,
        body: Any = _UNSET,
        headers: Mapping[str, str] | None = None,
        request_options: RequestOptions | None = None,
    ) -> httpx.Response:
        if request_options is not None and not isinstance(request_options, RequestOptions):
            raise TypeError("request_options must be a RequestOptions instance")
        response = self._send(
            self._build(method, path, query, body, headers, True, request_options),
            True,
            request_options,
        )
        self._raise_for_status(response, streamed=True)
        return response


def _backoff_seconds(attempt: int, retry_after: str | None) -> float:
    if retry_after:
        # Retry-After is either delta-seconds or an HTTP-date.
        try:
            seconds = float(retry_after)
            if seconds >= 0:
                return min(seconds, 60.0)
        except ValueError:
            try:
                from email.utils import parsedate_to_datetime

                at = parsedate_to_datetime(retry_after)
                seconds = at.timestamp() - time.time()
                if seconds > 0:
                    return min(seconds, 60.0)
            except (TypeError, ValueError):
                pass
    base = 0.5 * (2**attempt)
    return min(base, 8.0) * (0.5 + random.random() / 2)
