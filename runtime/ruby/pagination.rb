# frozen_string_literal: true

# Auto-iterating cursor page. `each` walks every item across pages, fetching
# lazily; `items` is just the current page.

module RedwoodModule
  class Page
    include Enumerable

    attr_reader :items, :next_cursor

    def initialize(items, next_cursor, &fetch)
      @items = items
      @next_cursor = next_cursor
      @fetch = fetch
    end

    def next_page?
      !@next_cursor.to_s.empty?
    end

    # The next page, or nil when this is the last one.
    def next_page
      return nil unless next_page?

      @fetch.call(@next_cursor)
    end

    def each(&block)
      return enum_for(:each) unless block_given?

      page = self
      until page.nil?
        page.items.each(&block)
        page = page.next_page
      end
      # Ruby collection iterators return the receiver in block form.
      self
    end

    alias auto_paging_each each
  end
end
