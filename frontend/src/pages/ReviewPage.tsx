import { useState, useEffect, useCallback } from 'react';
import type {
  ReviewItem,
  ReviewStatus,
  DetectorKind,
  ReviewDecision,
  DailyRollup,
  PatrolState,
} from '../api/types';
import { api } from '../api/client';
import { LoadingSpinner } from '../components/LoadingSpinner';
import { EmptyState } from '../components/EmptyState';

interface ReviewPageProps {
  showToast: (message: string, type: 'success' | 'error') => void;
}

const KIND_LABEL: Record<DetectorKind, string> = {
  staleness: 'Stale',
  duplicate: 'Duplicate',
  orphan: 'Orphan',
  external_mismatch: 'Out of sync',
};

const KIND_COLOR: Record<DetectorKind, string> = {
  staleness: 'bg-yellow-500/20 text-yellow-300',
  duplicate: 'bg-purple-500/20 text-purple-300',
  orphan: 'bg-blue-500/20 text-blue-300',
  external_mismatch: 'bg-orange-500/20 text-orange-300',
};

const STATUS_FILTERS: { value: ReviewStatus | 'all'; label: string }[] = [
  { value: 'pending', label: 'Pending' },
  { value: 'valid', label: 'Valid' },
  { value: 'needs_fix', label: 'Needs fix' },
  { value: 'deleted', label: 'Deleted' },
  { value: 'dismissed', label: 'Dismissed' },
  { value: 'all', label: 'All' },
];

function MetricCard({ label, value, hint }: { label: string; value: string | number; hint?: string }) {
  return (
    <div className="bg-card border border-border rounded-lg p-4">
      <div className="text-2xl font-bold text-gray-200">{value}</div>
      <div className="text-sm text-muted">{label}</div>
      {hint && <div className="text-xs text-muted mt-1">{hint}</div>}
    </div>
  );
}

function MetricCards({ rollup }: { rollup: DailyRollup }) {
  const zeroPct = `${Math.round(rollup.search.zero_result_rate * 100)}%`;
  return (
    <div className="grid grid-cols-2 md:grid-cols-3 gap-3 mb-6">
      <MetricCard label="Documents" value={rollup.ingest.document_count} />
      <MetricCard label="Graph edges" value={rollup.graph.edges} hint={`avg degree ${rollup.graph.avg_degree.toFixed(2)}`} />
      <MetricCard label="Orphans" value={rollup.graph.orphans} hint="no connections" />
      <MetricCard label="Open reviews" value={rollup.patrol.open_items} />
      <MetricCard label="Search zero-result" value={zeroPct} hint={`${rollup.search.count} searches`} />
      <MetricCard label="Detections" value={rollup.patrol.detections} hint={`${rollup.date}`} />
    </div>
  );
}

/** evidence(근거 수치)를 key: value 목록으로 간단히 렌더. */
function Evidence({ evidence }: { evidence: Record<string, unknown> }) {
  const entries = Object.entries(evidence);
  if (entries.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-x-4 gap-y-1 mt-2 text-xs text-muted">
      {entries.map(([k, v]) => (
        <span key={k}>
          <span className="text-gray-400">{k}:</span> {String(v)}
        </span>
      ))}
    </div>
  );
}

function ReviewCard({
  item,
  busy,
  onJudge,
}: {
  item: ReviewItem;
  busy: boolean;
  onJudge: (item: ReviewItem, decision: ReviewDecision) => void;
}) {
  const isPending = item.status === 'pending';
  return (
    <div className="bg-card border border-border rounded-lg p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="flex-1">
          <div className="flex items-center gap-2 mb-1">
            <span className={`text-xs px-2 py-0.5 rounded ${KIND_COLOR[item.kind]}`}>
              {KIND_LABEL[item.kind]}
            </span>
            {!isPending && (
              <span className="text-xs px-2 py-0.5 rounded bg-border text-muted capitalize">
                {item.status.replace('_', ' ')}
              </span>
            )}
            {!isPending && item.decided_by && (
              <span
                className={`text-xs px-2 py-0.5 rounded ${
                  item.decided_by === 'auto'
                    ? 'bg-primary/20 text-primary'
                    : 'bg-border text-muted'
                }`}
                title={item.decided_by === 'auto' ? 'AI가 자동 판정' : '사람이 판단'}
              >
                {item.decided_by === 'auto' ? 'Auto' : 'Human'}
              </span>
            )}
          </div>
          <p className="text-sm text-gray-200">{item.reason}</p>
          {!isPending && item.decision_reason && (
            <p className="text-xs text-muted mt-1 italic">“{item.decision_reason}”</p>
          )}
          <Evidence evidence={item.evidence} />
          <div className="text-xs text-muted mt-2 font-mono">doc {item.document_id.slice(0, 8)}</div>
        </div>
      </div>

      {isPending && (
        <div className="flex flex-wrap gap-2 mt-3">
          <button
            className="px-3 py-1.5 text-sm rounded bg-success/80 text-white hover:bg-success transition-colors disabled:opacity-50"
            onClick={() => onJudge(item, 'valid')}
            disabled={busy}
          >
            Valid
          </button>
          <button
            className="px-3 py-1.5 text-sm rounded bg-border text-gray-200 hover:bg-muted transition-colors disabled:opacity-50"
            onClick={() => onJudge(item, 'needs_fix')}
            disabled={busy}
          >
            Needs fix
          </button>
          <button
            className="px-3 py-1.5 text-sm rounded bg-error/80 text-white hover:bg-error transition-colors disabled:opacity-50"
            onClick={() => onJudge(item, 'deleted')}
            disabled={busy}
          >
            Delete
          </button>
          <button
            className="px-3 py-1.5 text-sm rounded bg-border text-gray-200 hover:bg-muted transition-colors disabled:opacity-50"
            onClick={() => onJudge(item, 'dismissed')}
            disabled={busy}
          >
            Dismiss
          </button>
        </div>
      )}
    </div>
  );
}

export function ReviewPage({ showToast }: ReviewPageProps) {
  const [reviews, setReviews] = useState<ReviewItem[]>([]);
  const [metrics, setMetrics] = useState<DailyRollup | null>(null);
  const [history, setHistory] = useState<PatrolState | null>(null);
  const [filter, setFilter] = useState<ReviewStatus | 'all'>('pending');
  const [isLoading, setIsLoading] = useState(true);
  const [isRunning, setIsRunning] = useState(false);
  const [judging, setJudging] = useState<Set<string>>(new Set());

  const load = useCallback(async () => {
    setIsLoading(true);
    try {
      const status = filter === 'all' ? undefined : filter;
      const [items, rollups, hist] = await Promise.all([
        api.listReviews(status),
        api.getMetrics().catch(() => [] as DailyRollup[]),
        api.getPatrolHistory().catch(() => null),
      ]);
      setReviews(items);
      setMetrics(rollups.length > 0 ? rollups[rollups.length - 1] : null);
      setHistory(hist);
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to load review queue', 'error');
    } finally {
      setIsLoading(false);
    }
  }, [filter, showToast]);

  useEffect(() => {
    load();
  }, [load]);

  const handleRun = async () => {
    setIsRunning(true);
    try {
      const run = await api.runPatrol();
      showToast(
        `Patrol done — ${run.detections.total} detected, ${run.enqueued} new, ${run.edges_decayed} edges decayed`,
        'success',
      );
      await load();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Patrol failed', 'error');
    } finally {
      setIsRunning(false);
    }
  };

  const handleJudge = async (item: ReviewItem, decision: ReviewDecision) => {
    if (decision === 'deleted' && !confirm('Delete this document? It stays recoverable via version history.')) {
      return;
    }
    setJudging((prev) => new Set(prev).add(item.id));
    try {
      await api.judgeReviews([item.id], decision);
      showToast(`Marked as ${decision.replace('_', ' ')}`, 'success');
      await load();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Judgment failed', 'error');
    } finally {
      setJudging((prev) => {
        const next = new Set(prev);
        next.delete(item.id);
        return next;
      });
    }
  };

  const lastRun = history?.last_run_at
    ? new Date(history.last_run_at).toLocaleString()
    : 'never';

  return (
    <div className="max-w-4xl mx-auto p-6">
      <div className="flex items-center justify-between mb-4">
        <div>
          <h2 className="text-xl font-semibold text-gray-200">Review Queue</h2>
          <p className="text-sm text-muted">Last patrol: {lastRun}</p>
        </div>
        <button
          className="px-4 py-2 bg-primary text-white rounded-lg hover:bg-primary-hover transition-colors disabled:opacity-50"
          onClick={handleRun}
          disabled={isRunning}
        >
          {isRunning ? 'Running…' : 'Run Patrol'}
        </button>
      </div>

      {metrics && <MetricCards rollup={metrics} />}

      {/* 상태 필터 */}
      <div className="flex gap-2 mb-4">
        {STATUS_FILTERS.map((f) => (
          <button
            key={f.value}
            className={`px-3 py-1.5 text-sm rounded transition-colors ${
              filter === f.value
                ? 'bg-primary text-white'
                : 'bg-border text-gray-200 hover:bg-muted'
            }`}
            onClick={() => setFilter(f.value)}
          >
            {f.label}
          </button>
        ))}
      </div>

      {isLoading ? (
        <LoadingSpinner text="Loading review queue…" />
      ) : reviews.length === 0 ? (
        <EmptyState
          title="Nothing to review"
          description="Run patrol to scan for stale, duplicate, orphan, and out-of-sync documents."
        />
      ) : (
        <div className="flex flex-col gap-3">
          {reviews.map((item) => (
            <ReviewCard
              key={item.id}
              item={item}
              busy={judging.has(item.id)}
              onJudge={handleJudge}
            />
          ))}
        </div>
      )}
    </div>
  );
}
