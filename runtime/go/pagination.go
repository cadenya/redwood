// Vendored runtime: cursor pagination.

import "context"

// Page is one page of results plus the means to fetch the next.
type Page[T any] struct {
	Items      []T
	NextCursor string

	fetch func(ctx context.Context, cursor string) (*Page[T], error)
}

func newPage[T any](items []T, nextCursor string, fetch func(ctx context.Context, cursor string) (*Page[T], error)) *Page[T] {
	return &Page[T]{Items: items, NextCursor: nextCursor, fetch: fetch}
}

// HasNextPage reports whether another page exists.
func (p *Page[T]) HasNextPage() bool { return p.NextCursor != "" }

// GetNextPage fetches the next page, or returns nil when exhausted.
func (p *Page[T]) GetNextPage(ctx context.Context) (*Page[T], error) {
	if !p.HasNextPage() {
		return nil, nil
	}
	return p.fetch(ctx, p.NextCursor)
}

// All walks every remaining page and collects the items.
func (p *Page[T]) All(ctx context.Context) ([]T, error) {
	var out []T
	page := p
	for page != nil {
		out = append(out, page.Items...)
		next, err := page.GetNextPage(ctx)
		if err != nil {
			return out, err
		}
		page = next
	}
	return out, nil
}
