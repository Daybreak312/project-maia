import { useState, useEffect } from 'react';
import type { Document, IngestResponse } from '../api/types';
import { api } from '../api/client';
import { AccordionItem } from '../components/AccordionItem';
import { LoadingSpinner } from '../components/LoadingSpinner';
import { EmptyState } from '../components/EmptyState';

interface AddPageProps {
  showToast: (message: string, type: 'success' | 'error') => void;
}

const STRATEGY_LABELS: Record<string, string> = {
  new: '새 문서로 저장',
  update: '기존 문서 업데이트 (이전 버전 보관)',
  split: '여러 문서로 분할',
  duplicate: '중복 감지 (원문 보관, 원본과 연결)',
  raw: 'raw 저장 (에이전트 우회)',
};

/** ingest 응답의 에이전트 판단 결과를 요약해 보여주는 배너. */
function StrategyBanner({ outcome }: { outcome: IngestResponse }) {
  const label = STRATEGY_LABELS[outcome.strategy!] ?? outcome.strategy;
  return (
    <div
      className={`mt-4 p-3 rounded-lg border text-sm ${
        outcome.fallback ? 'border-error/50 bg-error/10' : 'border-primary/30 bg-primary/5'
      }`}
    >
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-xs uppercase bg-primary text-white px-2 py-0.5 rounded">
          {outcome.strategy}
        </span>
        <span className="text-gray-200">{label}</span>
        {outcome.document_ids && outcome.document_ids.length > 1 && (
          <span className="text-muted">· 문서 {outcome.document_ids.length}개</span>
        )}
        {typeof outcome.edges_created === 'number' && outcome.edges_created > 0 && (
          <span className="text-muted">· 엣지 {outcome.edges_created}개</span>
        )}
        {outcome.fallback && (
          <span className="text-error">⚠ 판단 실패로 raw 저장됨 (정보 유실 없음)</span>
        )}
      </div>
      {outcome.reason && <p className="text-muted mt-1 italic">{outcome.reason}</p>}
    </div>
  );
}

export function AddPage({ showToast }: AddPageProps) {
  const [content, setContent] = useState('');
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [recentDocs, setRecentDocs] = useState<Document[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [lastOutcome, setLastOutcome] = useState<IngestResponse | null>(null);

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
      setLastOutcome(result);

      const note = result.strategy && result.strategy !== 'raw' ? ` (${result.strategy})` : '';
      showToast(`Entry processed${note}`, 'success');
      setContent('');

      // 전략(분할/업데이트/중복)에 따라 문서가 여러 개 생기거나 기존 문서가 바뀌므로,
      // 낙관적 추가 대신 목록을 다시 불러 정확한 상태를 반영한다.
      await loadRecent();
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
          ? { ...doc, summary: data.summary, entities: data.entities }
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
          placeholder="Enter information to store... (anything you want to remember)&#10;&#10;Ctrl+Enter (⌘+Enter) to submit"
          value={content}
          onChange={(e) => setContent(e.target.value)}
          onKeyDown={(e) => {
            if ((e.ctrlKey || e.metaKey) && e.key === 'Enter' && content.trim() && !isSubmitting) {
              e.preventDefault();
              handleSubmit(e);
            }
          }}
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

        {/* 에이전트 판단 결과 배너 */}
        {lastOutcome?.strategy && <StrategyBanner outcome={lastOutcome} />}
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
