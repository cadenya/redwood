# Auto-iterating cursor page. Iterating a SyncPage walks every item across
# pages, fetching lazily; `.items` is just the current page.
from __future__ import annotations

from typing import Callable, Generic, Iterator, List, TypeVar

T = TypeVar("T")


class SyncPage(Generic[T]):
    def __init__(
        self,
        items: List[T],
        next_cursor: str,
        fetch: Callable[[str], "SyncPage[T]"],
    ) -> None:
        self.items = items
        self.next_cursor = next_cursor
        self._fetch = fetch

    def has_next_page(self) -> bool:
        return bool(self.next_cursor)

    def get_next_page(self) -> "SyncPage[T] | None":
        """The next page, or None when this is the last one."""
        if not self.next_cursor:
            return None
        return self._fetch(self.next_cursor)

    def __iter__(self) -> Iterator[T]:
        page: SyncPage[T] | None = self
        while page is not None:
            yield from page.items
            page = page.get_next_page()
