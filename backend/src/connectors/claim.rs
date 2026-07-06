//! 문자열 키에 대한 원자적 in-flight 클레임(RAII) — 커넥터 실행 동시성의 최소 원자 단위.
//!
//! 두 입도에서 재사용된다(잠금 입도를 방어 대상 입도와 일치시키는 것이 핵심):
//! - **커넥터 단위**(`{workspace}/{connector_id}`): 같은 커넥터의 동시 sync를 막는다
//!   (수동 트리거 이중 클릭·스케줄러와 수동 트리거 겹침).
//! - **소스 단위**(`{workspace}⋯{source_type}⋯{source_id}`): 대상이 겹치는 서로 다른 커넥터가
//!   같은 파일을 동시에 유입해 UUID가 다른 중복 문서를 만드는 것을 막는다. dedup 키
//!   `(source_type, source_id)`와 입도를 일치시켜 `find_by_source→저장` TOCTOU 창을 닫는다.
//!
//! **왜 RAII(Drop) 가드인가:** claim 이후 어떤 경로(정상 종료·`?` 조기 반환·패닉 되감기·태스크
//! 취소)로 벗어나도 `Drop`이 키를 해제하므로 "해제 누락으로 영구 잠김"이 원천 불가능하다.
//! 수동 플래그 리셋은 조기 반환 한 번으로 키가 영영 남아 이후 모든 실행이 잠기는 더 나쁜 버그를
//! 낳는다.
//!
//! **왜 `std::sync::Mutex`인가:** 해제가 `Drop`에서 일어나야 하는데 `Drop`은 async가 불가하므로
//! `tokio::sync` 락을 쓸 수 없다. 크리티컬 섹션은 `insert`/`remove`/`contains`뿐이라 await를
//! 가로지르지 않으므로 std Mutex가 정확하고 값싸다.

use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};

/// 여러 키의 in-flight 상태를 관리하는 공유 집합. `Clone`은 같은 내부 상태를 가리킨다
/// (Arc 공유) — 스케줄러와 API가 하나의 집합을 공유해야 상호배제가 성립한다.
#[derive(Clone, Default)]
pub struct InFlightSet {
    keys: Arc<StdMutex<HashSet<String>>>,
}

impl InFlightSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// `key`를 원자적으로 claim한다. 이미 claim되어 있으면(=in-flight) `None`.
    ///
    /// 반환된 [`InFlightClaim`]이 Drop될 때 키가 해제된다 — 모든 종료 경로에서 자동.
    pub fn try_claim(&self, key: String) -> Option<InFlightClaim> {
        // HashSet::insert는 이미 존재하면 false — 이 한 줄이 원자적 검사-후-설정이다.
        // (poison은 크리티컬 섹션에 패닉 지점이 없어 사실상 불가하나, 방어적으로 복구한다.)
        let mut set = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        if !set.insert(key.clone()) {
            return None;
        }
        Some(InFlightClaim {
            keys: self.keys.clone(),
            key,
        })
    }

    /// `key`가 현재 claim되어 있는지 — **조언(advisory)** 조회.
    ///
    /// 빠른 사전 필터(스케줄러가 실행 중 커넥터 틱 스킵, API 409 즉답)에 쓴다. 무결성 *보증*은
    /// 언제나 `try_claim`의 원자적 claim이며, 이 조회는 그 앞단의 힌트일 뿐이다(조회 후 claim
    /// 사이의 미세 경합은 claim이 최종 판정).
    pub fn contains(&self, key: &str) -> bool {
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(key)
    }
}

/// [`InFlightSet::try_claim`]이 돌려주는 RAII 가드. Drop되면 claim한 키를 해제한다.
pub struct InFlightClaim {
    keys: Arc<StdMutex<HashSet<String>>>,
    key: String,
}

impl Drop for InFlightClaim {
    fn drop(&mut self) {
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_key_is_mutually_exclusive() {
        let set = InFlightSet::new();
        let c1 = set.try_claim("k".to_string());
        assert!(c1.is_some(), "첫 claim 성공");
        assert!(
            set.try_claim("k".to_string()).is_none(),
            "같은 키 재claim 실패(상호배제)"
        );
        assert!(set.contains("k"), "claim 보유 중 contains=true");
    }

    #[test]
    fn test_different_keys_are_independent() {
        let set = InFlightSet::new();
        let _a = set.try_claim("a".to_string()).unwrap();
        assert!(
            set.try_claim("b".to_string()).is_some(),
            "다른 키는 동시 claim 가능(격리)"
        );
    }

    #[test]
    fn test_drop_releases_key() {
        let set = InFlightSet::new();
        let c = set.try_claim("k".to_string());
        assert!(set.contains("k"));
        drop(c);
        assert!(!set.contains("k"), "Drop 후 키 해제");
        assert!(
            set.try_claim("k".to_string()).is_some(),
            "해제된 키 재claim 가능"
        );
    }

    #[test]
    fn test_cloned_set_shares_state() {
        // Clone된 핸들이 같은 내부 상태를 가리켜야 상호배제가 성립한다(스케줄러+API 공유 전제).
        let a = InFlightSet::new();
        let b = a.clone();
        let _c = a.try_claim("shared".to_string()).unwrap();
        assert!(
            b.try_claim("shared".to_string()).is_none(),
            "clone된 핸들에서도 같은 키는 배타적"
        );
        assert!(b.contains("shared"), "clone된 핸들이 같은 상태를 관측");
    }
}
