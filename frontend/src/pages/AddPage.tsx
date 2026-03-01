import { useState, useEffect } from 'react';
import type { Document, IngestResponse } from '../api/types';
import { api } from '../api/client';
import { AccordionItem } from '../components/AccordionItem';
import { LoadingSpinner } from '../components/LoadingSpinner';
import { EmptyState } from '../components/EmptyState';

interface AddPageProps {
  showToast: (message: string, type: 'success' | 'error') => void;
}

export function AddPage({ showToast }: AddPageProps) {
  const [content, setContent] = useState('');
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [recentDocs, setRecentDocs] = useState<Document[]>([]);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    loadRecent();
  }, []);

  const loadRecent = async () => {
    try {
      const response = await api.getRecent({ limit: 10 });
      setRecentDocs(response.documents);
    } catch (err) {
      console.error('Failed to load recent:', err);
    } finally {
      setIsLoading(false);
    }
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!content.trim()) return;

    setIsSubmitting(true);
    try {
      const result = await api.ingest(content.trim());
      showToast('Entry added successfully', 'success');
      setContent('');

      // Add to recent list
      const newDoc: Document = {
        id: result.id,
        raw_content: content.trim(),
        summary: result.summary,
        tags: result.tags,
        entities: result.entities,
        created_at: new Date().toISOString(),
      };
      setRecentDocs((prev) => [newDoc, ...prev.slice(0, 9)]);
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to add entry', 'error');
    } finally {
      setIsSubmitting(false);
    }
  };

  const handleUpdate = (id: string, data: IngestResponse) => {
    setRecentDocs((prev) =>
      prev.map((doc) =>
        doc.id === id
          ? { ...doc, summary: data.summary, tags: data.tags, entities: data.entities }
          : doc
      )
    );
  };

  const handleDelete = (id: string) => {
    setRecentDocs((prev) => prev.filter((doc) => doc.id !== id));
  };

  return (
    <div className="max-w-4xl mx-auto p-6">
      {/* Input Form */}
      <form onSubmit={handleSubmit} className="mb-8">
        <h2 className="text-xl font-semibold text-gray-200 mb-4">Add New Entry</h2>
        <textarea
          className="w-full min-h-[200px] bg-card border border-border rounded-lg p-4 text-gray-200 resize-y focus:outline-none focus:border-primary mb-4"
          placeholder="Enter information to store... (anything you want to remember)"
          value={content}
          onChange={(e) => setContent(e.target.value)}
          disabled={isSubmitting}
        />
        <button
          type="submit"
          className="w-full py-3 bg-primary text-white rounded-lg font-medium hover:bg-primary-hover transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center justify-center gap-2"
          disabled={isSubmitting || !content.trim()}
        >
          {isSubmitting ? (
            <>
              <span className="w-4 h-4 border-2 border-white border-t-transparent rounded-full animate-spin" />
              Processing...
            </>
          ) : (
            'Add Entry'
          )}
        </button>
      </form>

      {/* Recent Entries */}
      <div>
        <h2 className="text-xl font-semibold text-gray-200 mb-4">Recent Entries</h2>
        {isLoading ? (
          <LoadingSpinner text="Loading recent entries..." />
        ) : recentDocs.length === 0 ? (
          <EmptyState
            title="No entries yet"
            description="Start by adding your first entry above"
          />
        ) : (
          <div className="flex flex-col gap-3">
            {recentDocs.map((doc) => (
              <AccordionItem
                key={doc.id}
                document={doc}
                onUpdate={(data) => handleUpdate(doc.id, data)}
                onDelete={() => handleDelete(doc.id)}
                showToast={showToast}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
