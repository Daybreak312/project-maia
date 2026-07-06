import { useState, useRef } from 'react';
import type { Document, SearchResult, IngestResponse } from '../api/types';
import { api } from '../api/client';
import { AccordionItem } from '../components/AccordionItem';
import { SearchModeSelector } from '../components/SearchModeSelector';
import { Pagination } from '../components/Pagination';
import { LoadingSpinner } from '../components/LoadingSpinner';
import { EmptyState } from '../components/EmptyState';

interface SearchPageProps {
  showToast: (message: string, type: 'success' | 'error') => void;
}

type SearchMode = 'hybrid' | 'vector' | 'keyword';

interface SearchResultWithDoc extends SearchResult {
  document?: Document;
}

const PAGE_SIZE = 10;

export function SearchPage({ showToast }: SearchPageProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState('');
  const [mode, setMode] = useState<SearchMode>('hybrid');
  const [isSearching, setIsSearching] = useState(false);
  const [results, setResults] = useState<SearchResultWithDoc[]>([]);
  const [total, setTotal] = useState(0);
  const [offset, setOffset] = useState(0);
  const [hasSearched, setHasSearched] = useState(false);
  const [searchMode, setSearchMode] = useState('');

  const handleSearch = async (newOffset = 0) => {
    if (!query.trim()) return;

    setIsSearching(true);
    setHasSearched(true);
    setOffset(newOffset);

    try {
      const response = await api.search({
        query: query.trim(),
        limit: PAGE_SIZE,
        offset: newOffset,
        mode,
      });

      setTotal(response.total);
      setSearchMode(response.mode);

      // Fetch full documents for each result
      const resultsWithDocs = await Promise.all(
        response.results.map(async (result) => {
          try {
            const doc = await api.getDocument(result.id);
            return { ...result, document: doc };
          } catch {
            return result;
          }
        })
      );

      setResults(resultsWithDocs);
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Search failed', 'error');
      setResults([]);
      setTotal(0);
    } finally {
      setIsSearching(false);
      inputRef.current?.focus();
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    handleSearch(0);
  };

  const handleUpdate = (id: string, data: IngestResponse) => {
    setResults((prev) =>
      prev.map((result) =>
        result.id === id && result.document
          ? {
              ...result,
              summary: data.summary,
              document: {
                ...result.document,
                summary: data.summary,
                entities: data.entities,
              },
            }
          : result
      )
    );
  };

  const handleDelete = (id: string) => {
    setResults((prev) => prev.filter((result) => result.id !== id));
    setTotal((prev) => prev - 1);
  };

  return (
    <div className="max-w-4xl mx-auto p-6">
      {/* Search Form */}
      <form onSubmit={handleSubmit} className="mb-6">
        <h2 className="text-xl font-semibold text-gray-200 mb-4">Search</h2>
        <div className="flex gap-3 mb-4">
          <input
            ref={inputRef}
            type="text"
            className="flex-1 bg-card border border-border rounded-lg px-4 py-3 text-gray-200 focus:outline-none focus:border-primary"
            placeholder="Search your entries..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            disabled={isSearching}
          />
          <button
            type="submit"
            className="px-6 py-3 bg-primary text-white rounded-lg font-medium hover:bg-primary-hover transition-colors disabled:opacity-50"
            disabled={isSearching || !query.trim()}
          >
            Search
          </button>
        </div>

        {/* Search Options */}
        <div className="flex flex-wrap items-center gap-4">
          <SearchModeSelector mode={mode} onModeChange={setMode} />
        </div>
      </form>

      {/* Search Results */}
      <div>
        {isSearching ? (
          <LoadingSpinner text="Searching..." />
        ) : hasSearched && results.length === 0 ? (
          <EmptyState
            title="No results found"
            description={`No entries match "${query}" with the current filters`}
          />
        ) : results.length > 0 ? (
          <>
            <div className="flex justify-between items-center mb-4">
              <h3 className="text-lg font-medium text-gray-200">
                {total} result{total !== 1 ? 's' : ''} found
              </h3>
              <span className="text-sm text-muted">Mode: {searchMode}</span>
            </div>
            <div className="flex flex-col gap-3">
              {results.map((result, index) =>
                result.document ? (
                  <AccordionItem
                    key={result.id}
                    document={result.document}
                    rank={offset + index + 1}
                    score={result.relevance_score}
                    onUpdate={(data) => handleUpdate(result.id, data)}
                    onDelete={() => handleDelete(result.id)}
                    showToast={showToast}
                  />
                ) : null
              )}
            </div>
            <Pagination
              total={total}
              limit={PAGE_SIZE}
              offset={offset}
              onPageChange={(newOffset) => handleSearch(newOffset)}
            />
          </>
        ) : null}
      </div>
    </div>
  );
}
