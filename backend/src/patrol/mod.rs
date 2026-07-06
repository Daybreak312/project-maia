//! Patrol — 자기 관리 & 메모리 거버넌스 (Phase 5).
//!
//! 두뇌가 스스로의 기억 상태를 점검하는 **반자율** 계층. 청소부가 아니라 관측·거버넌스다:
//! 시스템이 후보를 식별해 플래그를 세우고(Review Queue), 소유자가 판단하고, 그 피드백이
//! 축적된다. Patrol 자체는 **읽기 + 플래그 + 감쇠 재계산**만 하며 문서를 변경/삭제하지 않는다.
//!
//! 구조:
//! - [`decay`]: 엣지 시간 감쇠(수학적 유지보수, 사람 판단 불필요). 순수·멱등.
//! - [`detectors`]: staleness/중복/고아/외부 불일치 탐지기(LLM 없이 수치 신호 기반, 순수).
//! - [`review`]: Review Queue 모델·저장·중복 방지·판단 처리.
//! - [`freshness`]: "유효" 판단 시각 기준점(staleness 유예의 근거).
//! - [`feedback`]: 검색 "관련 없음" 피드백 수집(일 JSONL) + doc별 집계(staleness 신호).
//! - [`metrics`]: 일 단위 메트릭 롤업(검색/그래프/유입/Patrol) 순수 계산·저장.
//! - [`history`]: Patrol 실행 이력·마지막 실행 시각(스케줄 판단).
//! - [`scheduler`]: 주기 실행 + 오류 격리.

pub mod decay;
pub mod detectors;
pub mod freshness;
pub mod review;

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

/// 유형별 한 번의 Patrol 실행에서 생성할 Review 항목 상한(오탐 폭주 방어).
/// Patrol이 대체로 주기당 1회 도므로 사실상 유형별 일일 상한이다.
pub const DEFAULT_PER_TYPE_CAP: usize = 50;

/// JSON을 파일에 **원자적으로** 쓴다 — temp에 쓴 뒤 rename으로 교체(torn write → 부팅
/// 브릭/큐 유실 방지). 호출 측이 write_lock으로 직렬화하므로 고정 temp 이름이 안전하다.
pub(crate) async fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("patrol 저장 디렉토리 생성 실패")?;
    }
    let content = serde_json::to_string_pretty(value).context("patrol 상태 직렬화 실패")?;
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, content)
        .await
        .context("patrol temp 파일 쓰기 실패")?;
    tokio::fs::rename(&tmp, path)
        .await
        .context("patrol 원자적 교체(rename) 실패")?;
    Ok(())
}
