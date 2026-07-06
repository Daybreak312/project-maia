//! Review Queue — 탐지 후보를 사람이 판단하는 대기열.
//!
//! Patrol이 세운 플래그를 소유자가 유효/수정 필요/삭제/기각으로 판단한다. 상태 전이는
//! **대기(Pending) → 판단됨**이며, 판단은 **멱등**이다(같은 판단 재제출이 상태를 깨지 않음).
//!
//! 저장: 워크스페이스별 단일 JSON 파일(`workspaces/{id}/patrol/review_queue.json`)에
//! 항목 배열을 담고, read-modify-write를 write_lock으로 직렬화하며 원자적으로 교체한다.
//! 개인 규모에서 항목 수가 크지 않아 전체 로드/저장이 단순하고 안전하다.
//!
//! **주의:** 이 저장소는 항목 상태만 다룬다. "유효→freshness 갱신 / 삭제→문서 삭제" 같은
//! 부수효과는 Patrol 오케스트레이터가 조율한다(이 모듈은 indexer/freshness를 모른다).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::atomic_write_json;
use super::detectors::{DetectorKind, ReviewCandidate};

/// Review 항목의 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// 대기 — 아직 판단되지 않음.
    Pending,
    /// 유효 — 문서가 아직 쓸모 있음. freshness 갱신 + 유예.
    Valid,
    /// 수정 필요 — 사람이 손봐야 함(상태만 기록).
    NeedsFix,
    /// 삭제됨 — 문서가 (버전 보관으로 복구 가능하게) 삭제됨.
    Deleted,
    /// 기각 — 오탐. 조치 없음.
    Dismissed,
}

impl ReviewStatus {
    /// 열린(대기) 상태인가 — dedup·판단 전이의 기준.
    pub fn is_open(&self) -> bool {
        matches!(self, ReviewStatus::Pending)
    }
}

/// 판단 결정 — API 입력과 매핑되는, "대기가 아닌" 목표 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Valid,
    NeedsFix,
    Deleted,
    Dismissed,
}

impl ReviewDecision {
    pub fn to_status(self) -> ReviewStatus {
        match self {
            ReviewDecision::Valid => ReviewStatus::Valid,
            ReviewDecision::NeedsFix => ReviewStatus::NeedsFix,
            ReviewDecision::Deleted => ReviewStatus::Deleted,
            ReviewDecision::Dismissed => ReviewStatus::Dismissed,
        }
    }
}

/// Review Queue 한 항목.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    pub id: Uuid,
    pub workspace: String,
    pub document_id: Uuid,
    pub kind: DetectorKind,
    /// 사람이 읽는 사유.
    pub reason: String,
    /// 판단 근거 수치(탐지기가 남긴 것).
    pub evidence: serde_json::Value,
    pub status: ReviewStatus,
    pub created_at: DateTime<Utc>,
    /// 판단 시각(대기 중이면 None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
}

/// 판단 제출 결과 — 항목과 "이번 호출로 실제 전이했는지"를 함께 준다.
///
/// `transitioned`가 true일 때만 오케스트레이터가 부수효과(freshness/삭제)를 수행한다
/// — 이미 판단된 항목의 재제출은 false를 반환해 부수효과가 두 번 실행되지 않는다(멱등).
#[derive(Debug, Clone)]
pub struct JudgeOutcome {
    pub item: ReviewItem,
    pub transitioned: bool,
}

/// 워크스페이스별 Review Queue를 파일로 저장/로드한다.
pub struct ReviewQueueStore {
    data_dir: PathBuf,
    write_lock: Mutex<()>,
}

impl ReviewQueueStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            write_lock: Mutex::new(()),
        }
    }

    fn path(&self, workspace_id: &str) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("patrol")
            .join("review_queue.json")
    }

    /// 전체 항목을 로드한다(파일 없으면 빈 목록).
    pub async fn load(&self, workspace_id: &str) -> Result<Vec<ReviewItem>> {
        let path = self.path(workspace_id);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str(&content).context("review queue 파싱 실패"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e).context("review queue 읽기 실패"),
        }
    }

    /// 상태·유형 필터로 항목을 조회한다(최신 생성순).
    pub async fn list(
        &self,
        workspace_id: &str,
        status: Option<ReviewStatus>,
        kind: Option<DetectorKind>,
    ) -> Result<Vec<ReviewItem>> {
        let mut items = self.load(workspace_id).await?;
        items.retain(|i| status.is_none_or(|s| i.status == s) && kind.is_none_or(|k| i.kind == k));
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(items)
    }

    /// 탐지 후보들을 큐에 추가한다. 반환: 실제로 추가된 항목 수.
    ///
    /// **중복 방지:** 이미 **열린**(대기) 동일 (문서, 유형) 항목이 있으면 추가하지 않는다.
    /// **유형별 상한:** 이번 호출에서 유형당 `per_type_cap`개까지만 추가한다(오탐 폭주 방어).
    pub async fn enqueue(
        &self,
        workspace_id: &str,
        candidates: &[ReviewCandidate],
        now: DateTime<Utc>,
        per_type_cap: usize,
    ) -> Result<usize> {
        let _guard = self.write_lock.lock().await;
        let mut items = self.load(workspace_id).await?;

        // 열린 항목의 (문서, 유형) 집합 — dedup 기준.
        let mut open: HashSet<(Uuid, DetectorKind)> = items
            .iter()
            .filter(|i| i.status.is_open())
            .map(|i| (i.document_id, i.kind))
            .collect();
        let mut added_per_kind: HashMap<DetectorKind, usize> = HashMap::new();
        let mut added = 0usize;

        for c in candidates {
            let key = (c.document_id, c.kind);
            if open.contains(&key) {
                continue; // 이미 열린 동일 항목 — 중복 생성 금지
            }
            let count = added_per_kind.entry(c.kind).or_insert(0);
            if *count >= per_type_cap {
                continue; // 유형별 상한 도달
            }
            items.push(ReviewItem {
                id: Uuid::new_v4(),
                workspace: workspace_id.to_string(),
                document_id: c.document_id,
                kind: c.kind,
                reason: c.reason.clone(),
                evidence: c.evidence.clone(),
                status: ReviewStatus::Pending,
                created_at: now,
                decided_at: None,
            });
            open.insert(key);
            *count += 1;
            added += 1;
        }

        if added > 0 {
            atomic_write_json(&self.path(workspace_id), &items).await?;
        }
        Ok(added)
    }

    /// 단일 항목을 판단한다. 없으면 `Ok(None)`.
    ///
    /// **멱등·전이 규칙:** 대기(열린) 항목만 판단 상태로 전이한다(transitioned=true).
    /// 이미 판단된 항목은 상태를 바꾸지 않고 transitioned=false로 돌려준다 — 같은 판단
    /// 재제출도, 다른 판단 재제출도 최초 판단을 깨지 않는다(부수효과 이중 실행 방지).
    pub async fn judge(
        &self,
        workspace_id: &str,
        item_id: Uuid,
        decision: ReviewDecision,
        now: DateTime<Utc>,
    ) -> Result<Option<JudgeOutcome>> {
        let _guard = self.write_lock.lock().await;
        let mut items = self.load(workspace_id).await?;
        let Some(item) = items.iter_mut().find(|i| i.id == item_id) else {
            return Ok(None);
        };

        if !item.status.is_open() {
            // 이미 판단됨 — 상태 유지(멱등).
            return Ok(Some(JudgeOutcome {
                item: item.clone(),
                transitioned: false,
            }));
        }

        item.status = decision.to_status();
        item.decided_at = Some(now);
        let outcome = JudgeOutcome {
            item: item.clone(),
            transitioned: true,
        };
        atomic_write_json(&self.path(workspace_id), &items).await?;
        Ok(Some(outcome))
    }

    /// 여러 항목을 한 번의 read-modify-write로 일괄 판단한다(파일 쓰기 1회).
    /// 각 항목의 전이 규칙은 [`judge`]와 동일(멱등). 반환: 존재한 항목들의 결과.
    pub async fn judge_many(
        &self,
        workspace_id: &str,
        item_ids: &[Uuid],
        decision: ReviewDecision,
        now: DateTime<Utc>,
    ) -> Result<Vec<JudgeOutcome>> {
        let _guard = self.write_lock.lock().await;
        let mut items = self.load(workspace_id).await?;
        let id_set: HashSet<Uuid> = item_ids.iter().copied().collect();
        let mut outcomes = Vec::new();
        let mut dirty = false;

        for item in items.iter_mut() {
            if !id_set.contains(&item.id) {
                continue;
            }
            if item.status.is_open() {
                item.status = decision.to_status();
                item.decided_at = Some(now);
                dirty = true;
                outcomes.push(JudgeOutcome {
                    item: item.clone(),
                    transitioned: true,
                });
            } else {
                outcomes.push(JudgeOutcome {
                    item: item.clone(),
                    transitioned: false,
                });
            }
        }

        if dirty {
            atomic_write_json(&self.path(workspace_id), &items).await?;
        }
        Ok(outcomes)
    }

    /// 열린(대기) 항목 수 — 메트릭·관측용.
    pub async fn count_open(&self, workspace_id: &str) -> Result<usize> {
        Ok(self
            .load(workspace_id)
            .await?
            .iter()
            .filter(|i| i.status.is_open())
            .count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn candidate(doc: Uuid, kind: DetectorKind) -> ReviewCandidate {
        ReviewCandidate {
            document_id: doc,
            kind,
            reason: "사유".to_string(),
            evidence: json!({"x": 1}),
        }
    }

    async fn store() -> (TempDir, ReviewQueueStore) {
        let tmp = TempDir::new().unwrap();
        let s = ReviewQueueStore::new(tmp.path());
        (tmp, s)
    }

    #[tokio::test]
    async fn test_enqueue_and_load() {
        let (_t, s) = store().await;
        let doc = Uuid::new_v4();
        let added = s
            .enqueue("default", &[candidate(doc, DetectorKind::Orphan)], Utc::now(), 50)
            .await
            .unwrap();
        assert_eq!(added, 1);
        let items = s.load("default").await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, ReviewStatus::Pending);
        assert_eq!(items[0].document_id, doc);
    }

    #[tokio::test]
    async fn test_enqueue_dedups_open_same_doc_and_kind() {
        let (_t, s) = store().await;
        let doc = Uuid::new_v4();
        let c = candidate(doc, DetectorKind::Staleness);
        s.enqueue("default", &[c.clone()], Utc::now(), 50).await.unwrap();
        // 두 번째 실행에서 같은 (문서, 유형) 후보는 추가되지 않아야 한다.
        let added = s.enqueue("default", &[c], Utc::now(), 50).await.unwrap();
        assert_eq!(added, 0, "이미 열린 동일 항목은 중복 생성 금지");
        assert_eq!(s.load("default").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_enqueue_allows_different_kind_same_doc() {
        let (_t, s) = store().await;
        let doc = Uuid::new_v4();
        s.enqueue("default", &[candidate(doc, DetectorKind::Staleness)], Utc::now(), 50)
            .await
            .unwrap();
        let added = s
            .enqueue("default", &[candidate(doc, DetectorKind::Orphan)], Utc::now(), 50)
            .await
            .unwrap();
        assert_eq!(added, 1, "같은 문서라도 다른 유형은 별개 항목");
    }

    #[tokio::test]
    async fn test_enqueue_reflags_after_decision() {
        // 판단되어 닫힌 뒤에는 같은 (문서, 유형)이 다시 플래그될 수 있다(dedup은 열린 항목만).
        let (_t, s) = store().await;
        let doc = Uuid::new_v4();
        s.enqueue("default", &[candidate(doc, DetectorKind::Orphan)], Utc::now(), 50)
            .await
            .unwrap();
        let item_id = s.load("default").await.unwrap()[0].id;
        s.judge("default", item_id, ReviewDecision::Dismissed, Utc::now())
            .await
            .unwrap();
        let added = s
            .enqueue("default", &[candidate(doc, DetectorKind::Orphan)], Utc::now(), 50)
            .await
            .unwrap();
        assert_eq!(added, 1, "닫힌 항목은 dedup 대상이 아니므로 재플래그 가능");
    }

    #[tokio::test]
    async fn test_enqueue_per_type_cap() {
        let (_t, s) = store().await;
        let cands: Vec<ReviewCandidate> = (0..5)
            .map(|_| candidate(Uuid::new_v4(), DetectorKind::Duplicate))
            .collect();
        let added = s.enqueue("default", &cands, Utc::now(), 2).await.unwrap();
        assert_eq!(added, 2, "유형별 상한 2를 지켜야 한다");
    }

    #[tokio::test]
    async fn test_list_filters_by_status_and_kind() {
        let (_t, s) = store().await;
        let d1 = Uuid::new_v4();
        let d2 = Uuid::new_v4();
        s.enqueue(
            "default",
            &[candidate(d1, DetectorKind::Staleness), candidate(d2, DetectorKind::Orphan)],
            Utc::now(),
            50,
        )
        .await
        .unwrap();
        // 하나를 판단해 닫는다.
        let items = s.load("default").await.unwrap();
        let stale_id = items.iter().find(|i| i.kind == DetectorKind::Staleness).unwrap().id;
        s.judge("default", stale_id, ReviewDecision::Valid, Utc::now())
            .await
            .unwrap();

        assert_eq!(s.list("default", Some(ReviewStatus::Pending), None).await.unwrap().len(), 1);
        assert_eq!(s.list("default", Some(ReviewStatus::Valid), None).await.unwrap().len(), 1);
        assert_eq!(
            s.list("default", None, Some(DetectorKind::Orphan)).await.unwrap().len(),
            1
        );
        assert_eq!(s.list("default", None, None).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_judge_transitions_once() {
        let (_t, s) = store().await;
        let doc = Uuid::new_v4();
        s.enqueue("default", &[candidate(doc, DetectorKind::Orphan)], Utc::now(), 50)
            .await
            .unwrap();
        let id = s.load("default").await.unwrap()[0].id;

        let first = s.judge("default", id, ReviewDecision::Deleted, Utc::now()).await.unwrap().unwrap();
        assert!(first.transitioned, "대기 → 판단은 전이");
        assert_eq!(first.item.status, ReviewStatus::Deleted);
        assert!(first.item.decided_at.is_some());
    }

    #[tokio::test]
    async fn test_judge_idempotent_resubmit() {
        let (_t, s) = store().await;
        let doc = Uuid::new_v4();
        s.enqueue("default", &[candidate(doc, DetectorKind::Orphan)], Utc::now(), 50)
            .await
            .unwrap();
        let id = s.load("default").await.unwrap()[0].id;

        s.judge("default", id, ReviewDecision::Valid, Utc::now()).await.unwrap();
        // 같은 판단 재제출 — 전이 없음, 상태 유지(멱등).
        let again = s.judge("default", id, ReviewDecision::Valid, Utc::now()).await.unwrap().unwrap();
        assert!(!again.transitioned, "재제출은 전이하지 않는다");
        assert_eq!(again.item.status, ReviewStatus::Valid);

        // 다른 판단 재제출도 최초 판단을 덮어쓰지 않는다(보수적).
        let diff = s.judge("default", id, ReviewDecision::Deleted, Utc::now()).await.unwrap().unwrap();
        assert!(!diff.transitioned);
        assert_eq!(diff.item.status, ReviewStatus::Valid, "최초 판단 유지");
    }

    #[tokio::test]
    async fn test_judge_missing_item_returns_none() {
        let (_t, s) = store().await;
        let res = s.judge("default", Uuid::new_v4(), ReviewDecision::Valid, Utc::now()).await.unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn test_judge_many_bulk() {
        let (_t, s) = store().await;
        let cands: Vec<ReviewCandidate> = (0..3)
            .map(|_| candidate(Uuid::new_v4(), DetectorKind::Duplicate))
            .collect();
        s.enqueue("default", &cands, Utc::now(), 50).await.unwrap();
        let ids: Vec<Uuid> = s.load("default").await.unwrap().iter().map(|i| i.id).collect();

        let outcomes = s.judge_many("default", &ids, ReviewDecision::Dismissed, Utc::now()).await.unwrap();
        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.iter().all(|o| o.transitioned));
        assert_eq!(s.count_open("default").await.unwrap(), 0, "모두 닫혀야 한다");
    }

    #[tokio::test]
    async fn test_count_open() {
        let (_t, s) = store().await;
        let cands: Vec<ReviewCandidate> = (0..3)
            .map(|_| candidate(Uuid::new_v4(), DetectorKind::Orphan))
            .collect();
        s.enqueue("default", &cands, Utc::now(), 50).await.unwrap();
        assert_eq!(s.count_open("default").await.unwrap(), 3);
        let id = s.load("default").await.unwrap()[0].id;
        s.judge("default", id, ReviewDecision::Valid, Utc::now()).await.unwrap();
        assert_eq!(s.count_open("default").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_workspace_isolation() {
        let (_t, s) = store().await;
        s.enqueue("personal", &[candidate(Uuid::new_v4(), DetectorKind::Orphan)], Utc::now(), 50)
            .await
            .unwrap();
        assert_eq!(s.load("personal").await.unwrap().len(), 1);
        assert!(s.load("work").await.unwrap().is_empty());
    }
}
