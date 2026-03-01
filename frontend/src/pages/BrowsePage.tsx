import { useState, useEffect, useCallback } from 'react';
import type { Document, IngestResponse } from '../api/types';
import { api } from '../api/client';
import { AccordionItem } from '../components/AccordionItem';
import { TagFilter } from '../components/TagFilter';
import { Pagination } from '../components/Pagination';
import { LoadingSpinner } from '../components/LoadingSpinner';
import { EmptyState } from '../components/EmptyState';

interface BrowsePageProps {
  showToast: (message: string, type: 'success' | 'error') => void;
}

const PAGE_SIZE = 20;

export function BrowsePage({ showToast }: BrowsePageProps) {
  const [documents, setDocuments] = useState<Document[]>([]);
  const [total, setTotal] = useState(0);
  const [offset, setOffset] = useState(0);
  const [selectedTags, setSelectedTags] = useState<string[]>([]);
  const [isLoading, setIsLoading] = useState(true);

  const loadDocuments = useCallback(async (newOffset = 0) => {
    setIsLoading(true);
    setOffset(newOffset);

    try {
      const response = await api.getRecent({
        limit: PAGE_SIZE,
        offset: newOffset,
        tags: selectedTags.length > 0 ? selectedTags : undefined,
      });
      setDocuments(response.documents);
      setTotal(response.total);
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to load entries', 'error');
    } finally {
      setIsLoading(false);
    }
  }, [selectedTags, showToast]);

  useEffect(() => {
    loadDocuments(0);
  }, [selectedTags]); // Reload when tags change

  const handleUpdate = (id: string, data: IngestResponse) => {
    setDocuments((prev) =>
      prev.map((doc) =>
        doc.id === id
          ? { ...doc, summary: data.summary, tags: data.tags, entities: data.entities }
          : doc
      )
    );
  };

  const handleDelete = (id: string) => {
    setDocuments((prev) => prev.filter((doc) => doc.id !== id));
    setTotal((prev) => prev - 1);
  };

  return (
    <div className="max-w-4xl mx-auto p-6">
      <h2 className="text-xl font-semibold text-gray-200 mb-4">All Entries</h2>

      {/* Tag Filter */}
      <TagFilter selectedTags={selectedTags} onTagsChange={setSelectedTags} />

      {isLoading ? (
        <LoadingSpinner text="Loading entries..." />
      ) : documents.length === 0 ? (
        <EmptyState
          title={selectedTags.length > 0 ? 'No matching entries' : 'No entries yet'}
          description={
            selectedTags.length > 0
              ? 'Try removing some tag filters'
              : 'Add your first entry to get started'
          }
        />
      ) : (
        <>
          <div className="flex justify-between items-center mb-4">
            <p className="text-muted">
              Showing {offset + 1}-{Math.min(offset + documents.length, total)} of {total} entries
            </p>
          </div>
          <div className="flex flex-col gap-3">
            {documents.map((doc) => (
              <AccordionItem
                key={doc.id}
                document={doc}
                onUpdate={(data) => handleUpdate(doc.id, data)}
                onDelete={() => handleDelete(doc.id)}
                showToast={showToast}
              />
            ))}
          </div>
          <Pagination
            total={total}
            limit={PAGE_SIZE}
            offset={offset}
            onPageChange={(newOffset) => loadDocuments(newOffset)}
          />
        </>
      )}
    </div>
  );
}
