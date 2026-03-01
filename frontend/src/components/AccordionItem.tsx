import { useState } from 'react';
import type { Document, Entity, IngestResponse } from '../api/types';
import { getEntityTypeLabel } from '../api/types';
import { api } from '../api/client';

interface AccordionItemProps {
  document: Document;
  rank?: number;
  score?: number;
  onUpdate?: (data: IngestResponse) => void;
  onDelete?: () => void;
  showToast: (message: string, type: 'success' | 'error') => void;
}

function formatDate(isoString: string) {
  return new Date(isoString).toLocaleDateString('ko-KR', {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  });
}

function EntityItem({ entity }: { entity: Entity }) {
  return (
    <div className="flex items-center gap-3 bg-bg rounded-md px-3 py-2">
      <span className="text-xs uppercase bg-primary text-white px-2 py-0.5 rounded">
        {getEntityTypeLabel(entity.entity_type)}
      </span>
      <span className="font-medium">{entity.value}</span>
      {entity.context && (
        <span className="text-sm text-muted italic">{entity.context}</span>
      )}
    </div>
  );
}

export function AccordionItem({
  document: doc,
  rank,
  score,
  onUpdate,
  onDelete,
  showToast,
}: AccordionItemProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [isEditing, setIsEditing] = useState(false);
  const [editContent, setEditContent] = useState(doc.raw_content);
  const [isSaving, setIsSaving] = useState(false);

  const handleSave = async () => {
    if (!editContent.trim()) {
      showToast('Content cannot be empty', 'error');
      return;
    }

    setIsSaving(true);
    try {
      const result = await api.updateDocument(doc.id, editContent.trim());
      onUpdate?.(result);
      setIsEditing(false);
      showToast('Entry updated', 'success');
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Update failed', 'error');
    } finally {
      setIsSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!confirm('Delete this entry?')) return;

    try {
      await api.deleteDocument(doc.id);
      onDelete?.();
      showToast('Entry deleted', 'success');
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Delete failed', 'error');
    }
  };

  const handleCancel = () => {
    setEditContent(doc.raw_content);
    setIsEditing(false);
  };

  return (
    <div className="bg-card border border-border rounded-lg overflow-hidden">
      {/* Header */}
      <div
        className="flex justify-between items-center p-4 cursor-pointer hover:bg-white/[0.02] transition-colors"
        onClick={() => setIsOpen(!isOpen)}
      >
        <div className="flex items-center gap-3 flex-1 min-w-0">
          <span
            className={`text-muted text-sm transition-transform ${
              isOpen ? 'rotate-90' : ''
            }`}
          >
            ▶
          </span>
          {rank !== undefined && (
            <span className="text-primary font-semibold">#{rank}</span>
          )}
          <span className="truncate">{doc.summary}</span>
        </div>
        <div className="flex items-center gap-4 ml-4 shrink-0">
          {score !== undefined && (
            <span className="text-sm text-primary bg-primary/10 px-2 py-1 rounded">
              {Math.round(score * 100)}%
            </span>
          )}
          <span className="text-sm text-muted">{formatDate(doc.created_at)}</span>
        </div>
      </div>

      {/* Content */}
      {isOpen && (
        <div className="p-4 pt-0 border-t border-border">
          {/* Tags */}
          <div className="flex flex-wrap gap-2 mb-4">
            {doc.tags.map((tag, i) => (
              <span
                key={i}
                className="bg-primary/20 text-primary px-3 py-1 rounded-full text-sm"
              >
                {tag}
              </span>
            ))}
          </div>

          {/* Entities */}
          {doc.entities.length > 0 && (
            <div className="flex flex-col gap-2 mb-4">
              {doc.entities.map((entity, i) => (
                <EntityItem key={i} entity={entity} />
              ))}
            </div>
          )}

          {/* Raw Content */}
          <div className="mb-4">
            <label className="block text-xs uppercase text-muted mb-2">
              Raw Content
            </label>
            {isEditing ? (
              <textarea
                className="w-full min-h-[150px] bg-bg border border-border rounded-lg p-4 text-gray-200 resize-y focus:outline-none focus:border-primary"
                value={editContent}
                onChange={(e) => setEditContent(e.target.value)}
                autoFocus
              />
            ) : (
              <pre className="bg-bg border border-border rounded-lg p-4 whitespace-pre-wrap break-words text-sm max-h-[200px] overflow-y-auto">
                {doc.raw_content}
              </pre>
            )}
          </div>

          {/* Actions */}
          <div className="flex justify-end gap-2">
            {isEditing ? (
              <>
                <button
                  className="px-4 py-2 bg-border text-gray-200 rounded-lg hover:bg-muted transition-colors"
                  onClick={handleCancel}
                  disabled={isSaving}
                >
                  Cancel
                </button>
                <button
                  className="px-4 py-2 bg-primary text-white rounded-lg hover:bg-primary-hover transition-colors disabled:opacity-50"
                  onClick={handleSave}
                  disabled={isSaving}
                >
                  {isSaving ? 'Saving...' : 'Save'}
                </button>
              </>
            ) : (
              <>
                <button
                  className="px-4 py-2 bg-border text-gray-200 rounded-lg hover:bg-muted transition-colors"
                  onClick={() => setIsEditing(true)}
                >
                  Edit
                </button>
                <button
                  className="px-4 py-2 bg-error text-white rounded-lg hover:bg-red-700 transition-colors"
                  onClick={handleDelete}
                >
                  Delete
                </button>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
