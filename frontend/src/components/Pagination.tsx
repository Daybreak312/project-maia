interface PaginationProps {
  total: number;
  limit: number;
  offset: number;
  onPageChange: (offset: number) => void;
}

export function Pagination({ total, limit, offset, onPageChange }: PaginationProps) {
  const totalPages = Math.ceil(total / limit);
  const currentPage = Math.floor(offset / limit) + 1;

  if (totalPages <= 1) return null;

  const pages: (number | 'ellipsis')[] = [];
  for (let i = 1; i <= totalPages; i++) {
    if (
      i === 1 ||
      i === totalPages ||
      (i >= currentPage - 1 && i <= currentPage + 1)
    ) {
      pages.push(i);
    } else if (pages[pages.length - 1] !== 'ellipsis') {
      pages.push('ellipsis');
    }
  }

  return (
    <div className="flex items-center justify-center gap-2 mt-6">
      <button
        className="px-3 py-1 rounded bg-border text-gray-200 hover:bg-muted disabled:opacity-30 disabled:cursor-not-allowed"
        disabled={currentPage === 1}
        onClick={() => onPageChange(Math.max(0, offset - limit))}
      >
        Prev
      </button>

      {pages.map((page, idx) =>
        page === 'ellipsis' ? (
          <span key={`ellipsis-${idx}`} className="text-muted px-2">
            ...
          </span>
        ) : (
          <button
            key={page}
            className={`px-3 py-1 rounded transition-colors ${
              page === currentPage
                ? 'bg-primary text-white'
                : 'bg-border text-gray-200 hover:bg-muted'
            }`}
            onClick={() => onPageChange((page - 1) * limit)}
          >
            {page}
          </button>
        )
      )}

      <button
        className="px-3 py-1 rounded bg-border text-gray-200 hover:bg-muted disabled:opacity-30 disabled:cursor-not-allowed"
        disabled={currentPage === totalPages}
        onClick={() => onPageChange(offset + limit)}
      >
        Next
      </button>
    </div>
  );
}
