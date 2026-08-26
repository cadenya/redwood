/** Cursor pagination. Pages are async-iterable across page boundaries. */

export class Page<T> implements AsyncIterable<T> {
  constructor(
    readonly items: T[],
    readonly nextCursor: string | undefined,
    private readonly fetchPage: (cursor: string) => Promise<Page<T>>,
  ) {}

  hasNextPage(): boolean {
    return this.nextCursor !== undefined && this.nextCursor !== '';
  }

  async getNextPage(): Promise<Page<T> | null> {
    if (!this.hasNextPage()) return null;
    return this.fetchPage(this.nextCursor as string);
  }

  /** Iterate every item on every page, fetching lazily. */
  async *[Symbol.asyncIterator](): AsyncIterator<T> {
    let page: Page<T> | null = this;
    while (page) {
      for (const item of page.items) yield item;
      page = await page.getNextPage();
    }
  }
}
