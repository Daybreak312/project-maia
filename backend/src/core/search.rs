use std::collections::{HashMap, HashSet};
use unicode_segmentation::UnicodeSegmentation;

/// BM25 파라미터
const K1: f32 = 1.2;
const B: f32 = 0.75;

/// 토큰의 최소 길이 (그래핌 기준). 1글자 조사/자모/철자 노이즈 제거.
const MIN_TOKEN_GRAPHEMES: usize = 2;

/// 한글 런에서 생성할 접두사의 최대 길이(음절/그래핌 기준).
///
/// 한국어는 조사(에서/를/로)가 어간 **뒤**에 붙는 교착어이고 어간은 런 앞쪽에
/// 온다. 따라서 앞쪽 일부 접두사만으로 어간이 노출되며, 현실적인 한국어
/// 단어+조사 덩어리는 이 길이를 넘지 않는다(가장 긴 복합명사+중첩 조사도 여유).
///
/// 이 상한이 없으면 공백 없는 긴 한글 런(길이 N)에서 접두사 2..N개(총 O(N²)
/// 그래핌)를 생성해, 단일 검색/인덱싱 요청이 수 GB를 할당하고 인스턴스 전체를
/// OOM으로 다운시킨다(에이전트가 대용량 한글 텍스트를 그대로 질의/저장하는
/// 정상 사용 패턴에서 사고로도 트리거됨). 상한을 넘는 런의 원본 전체는 tokenize
/// 1단계에서 이미 토큰으로 보존되므로 정확 매칭 재현율은 유지된다.
const MAX_HANGUL_PREFIX_LEN: usize = 32;

/// 하나의 단어(공백·구두점으로 구분된 덩어리)가 생성할 수 있는 토큰 수 상한.
///
/// 현실적인 단어는 원본 1개 + 한글 접두사(≤MAX_HANGUL_PREFIX_LEN) + 소수의
/// 라틴 런으로 이 값 근처에도 오지 않는다. 상한의 목적은 스크립트가 교대되는
/// 긴 무공백 입력("가가x나나x…")이 push_unique의 dedup을 단어당 O(k²)로
/// 악화시켜 CPU를 장시간 점유하는 DoS를 차단하는 것이다. 상한에 도달하면 이후
/// 토큰은 무시되어 tokenize 전체가 입력 길이에 대해 선형(O(N))으로 유지된다.
const MAX_TOKENS_PER_WORD: usize = 128;

/// 문자가 한글(음절/자모)인지 판별한다.
fn is_hangul(c: char) -> bool {
    matches!(c,
        '\u{AC00}'..='\u{D7A3}'   // 한글 음절 (가~힣)
        | '\u{1100}'..='\u{11FF}' // 한글 자모
        | '\u{3130}'..='\u{318F}' // 호환용 자모
        | '\u{A960}'..='\u{A97F}' // 확장 자모 A
        | '\u{D7B0}'..='\u{D7FF}' // 확장 자모 B
    )
}

/// 길이 조건(그래핌 ≥ MIN_TOKEN_GRAPHEMES)을 통과하면 중복 없이 추가한다.
///
/// 한 단어 내부에서만 중복을 제거한다. 원본 전체 토큰과 한글 전체-접두사가
/// 동일할 때 같은 토큰이 두 번 들어가 tf(단어 빈도)가 왜곡되는 것을 막는다.
/// 서로 다른 단어에서 나온 같은 토큰은 정상적으로 누적되어야 하므로 전역
/// 중복 제거는 하지 않는다.
///
/// 단어당 토큰 수를 MAX_TOKENS_PER_WORD로 상한한다. 상한 검사는 선형 dedup
/// 스캔보다 **앞**에 두어, 스크립트가 교대되는 긴 무공백 입력에서도 단어당
/// 작업량이 O(k²)로 폭증하지 않고 O(1)로 조기 반환되게 한다(현실 단어는 상한에
/// 도달하지 않으므로 정상 경로는 영향 없음).
fn push_unique(acc: &mut Vec<String>, token: &str) {
    if token.graphemes(true).count() < MIN_TOKEN_GRAPHEMES {
        return;
    }
    if acc.len() >= MAX_TOKENS_PER_WORD {
        return;
    }
    if !acc.iter().any(|t| t == token) {
        acc.push(token.to_string());
    }
}

/// 단어를 스크립트(한글 vs 그 외)가 연속된 런으로 분리한다.
/// 예: "react와" -> [(false, "react"), (true, "와")]
fn script_runs(word: &str) -> Vec<(bool, String)> {
    let mut runs: Vec<(bool, String)> = Vec::new();
    for c in word.chars() {
        let kr = is_hangul(c);
        match runs.last_mut() {
            Some((last_kr, buf)) if *last_kr == kr => buf.push(c),
            _ => runs.push((kr, c.to_string())),
        }
    }
    runs
}

/// 한글 런의 길이 2 이상 접두사를 만든다 (상한 MAX_HANGUL_PREFIX_LEN까지).
///
/// 한국어 조사(에서/를/로 등)는 어간 뒤에 붙는 접미사이므로, 접두사 집합에는
/// 조사가 제거된 어간이 항상 포함된다. 예: "해커톤에서" -> [해커, 해커톤, 해커톤에, 해커톤에서].
///
/// 런이 상한보다 길면 앞쪽 상한 길이까지의 그래핌만 접두사로 만든다. 어간은 런
/// 앞쪽에 있어 상한 내에서 이미 노출되므로 재현율 손실은 사실상 없고, 이로써
/// 긴 한글 런에서의 O(N²) 메모리 폭발(단일 요청 OOM)을 차단한다.
fn hangul_prefixes(run: &str) -> Vec<String> {
    // 상한 길이까지만 수집한다 — 긴 런 전체를 char 벡터로 모으지 않아 메모리도 O(1).
    let chars: Vec<char> = run.chars().take(MAX_HANGUL_PREFIX_LEN).collect();
    (MIN_TOKEN_GRAPHEMES..=chars.len())
        .map(|len| chars[..len].iter().collect())
        .collect()
}

/// 키워드 검색을 위한 토큰화 (한국어 + 영어 혼합 지원).
///
/// 설계 근거:
/// - 한국어는 조사가 어간 **뒤**에 붙는 교착어다. 공백 단위로만 자르면
///   "해커톤에서"가 "해커톤" 질의와 매칭되지 않아 재현율이 붕괴한다. 따라서
///   한글 런은 접두사로 분해해 어간을 노출시킨다.
/// - 영어 단어는 이미 공백/구두점으로 구분되므로 런 전체를 그대로 쓴다
///   (접두 분해 시 "re", "rea" 같은 노이즈만 늘어난다).
/// - 원본 공백 토큰 전체도 유지해 "rust로"처럼 혼합된 정확 표현을 보존한다.
///
/// 형태소 분석기 없이 동작하는 근사치이며, 정밀 튜닝은 Phase 3 범위다.
pub fn tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut tokens = Vec::new();

    // 공백·구두점(비영숫자) 기준으로 단어를 나눈다. 한글은 영숫자로 취급되어
    // 유지되므로 "해커톤에서", "react와" 같은 덩어리가 하나의 단어로 들어온다.
    for word in lower.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }

        let mut word_tokens: Vec<String> = Vec::new();

        // 1) 원본 단어 전체 (혼합 토큰 정확 매칭 보존)
        push_unique(&mut word_tokens, word);

        // 2) 스크립트 런 단위 처리
        for (is_kr, run) in script_runs(word) {
            if is_kr {
                for prefix in hangul_prefixes(&run) {
                    push_unique(&mut word_tokens, &prefix);
                }
            } else {
                push_unique(&mut word_tokens, &run);
            }
        }

        tokens.extend(word_tokens);
    }

    tokens
}

/// 문서 컬렉션의 BM25 스코어 계산기
pub struct BM25Scorer {
    /// 문서별 토큰 빈도
    doc_token_freqs: HashMap<String, HashMap<String, usize>>,
    /// 전체 문서에서 토큰이 등장한 문서 수
    doc_freqs: HashMap<String, usize>,
    /// 문서별 길이
    doc_lengths: HashMap<String, usize>,
    /// 평균 문서 길이
    avg_doc_length: f32,
    /// 전체 문서 수
    total_docs: usize,
}

impl BM25Scorer {
    pub fn new() -> Self {
        Self {
            doc_token_freqs: HashMap::new(),
            doc_freqs: HashMap::new(),
            doc_lengths: HashMap::new(),
            avg_doc_length: 0.0,
            total_docs: 0,
        }
    }

    /// 문서 추가
    pub fn add_document(&mut self, doc_id: &str, text: &str) {
        let tokens = tokenize(text);
        let doc_length = tokens.len();

        // 토큰 빈도 계산
        let mut token_freq: HashMap<String, usize> = HashMap::new();
        for token in &tokens {
            *token_freq.entry(token.clone()).or_insert(0) += 1;
        }

        // 문서 빈도 업데이트 (새 토큰만)
        let unique_tokens: HashSet<_> = tokens.iter().cloned().collect();
        for token in unique_tokens {
            *self.doc_freqs.entry(token).or_insert(0) += 1;
        }

        self.doc_token_freqs.insert(doc_id.to_string(), token_freq);
        self.doc_lengths.insert(doc_id.to_string(), doc_length);
        self.total_docs += 1;

        // 평균 길이 재계산
        let total_length: usize = self.doc_lengths.values().sum();
        self.avg_doc_length = total_length as f32 / self.total_docs as f32;
    }

    /// 쿼리에 대한 문서별 BM25 스코어 계산
    pub fn score(&self, query: &str) -> Vec<(String, f32)> {
        let query_tokens = tokenize(query);
        let mut scores: HashMap<String, f32> = HashMap::new();

        for token in &query_tokens {
            let df = *self.doc_freqs.get(token).unwrap_or(&0) as f32;
            if df == 0.0 {
                continue;
            }

            // IDF 계산
            let idf = ((self.total_docs as f32 - df + 0.5) / (df + 0.5) + 1.0).ln();

            for (doc_id, token_freqs) in &self.doc_token_freqs {
                let tf = *token_freqs.get(token).unwrap_or(&0) as f32;
                if tf == 0.0 {
                    continue;
                }

                let doc_length = *self.doc_lengths.get(doc_id).unwrap_or(&1) as f32;
                let length_norm = 1.0 - B + B * (doc_length / self.avg_doc_length);

                // BM25 스코어
                let score = idf * (tf * (K1 + 1.0)) / (tf + K1 * length_norm);
                *scores.entry(doc_id.clone()).or_insert(0.0) += score;
            }
        }

        let mut results: Vec<_> = scores.into_iter().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }
}

/// RRF (Reciprocal Rank Fusion) - 여러 랭킹 리스트 결합
pub fn reciprocal_rank_fusion(
    rankings: Vec<Vec<(String, f32)>>,
    k: f32,
) -> Vec<(String, f32)> {
    let mut fused_scores: HashMap<String, f32> = HashMap::new();

    for ranking in rankings {
        for (rank, (doc_id, _)) in ranking.iter().enumerate() {
            let rrf_score = 1.0 / (k + rank as f32 + 1.0);
            *fused_scores.entry(doc_id.clone()).or_insert(0.0) += rrf_score;
        }
    }

    let mut results: Vec<_> = fused_scores.into_iter().collect();
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results
}

/// 검색 모드
#[derive(Debug, Clone, Copy, Default)]
pub enum SearchMode {
    /// 벡터 검색만
    Vector,
    /// 키워드 검색만
    Keyword,
    /// 하이브리드 (벡터 + 키워드 RRF)
    #[default]
    Hybrid,
}

impl std::str::FromStr for SearchMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "vector" => Ok(SearchMode::Vector),
            "keyword" => Ok(SearchMode::Keyword),
            "hybrid" => Ok(SearchMode::Hybrid),
            _ => Ok(SearchMode::Hybrid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_korean() {
        let tokens = tokenize("오늘 해커톤에서 재미있는 프로젝트를 만들었다");
        assert!(tokens.contains(&"오늘".to_string()));
        assert!(tokens.contains(&"해커톤에서".to_string()));
    }

    #[test]
    fn test_tokenize_mixed() {
        let tokens = tokenize("React와 Rust로 개발");
        assert!(tokens.contains(&"react".to_string()));
        assert!(tokens.contains(&"rust로".to_string()));
    }

    #[test]
    fn test_tokenize_korean_stem_exposed() {
        // 조사가 붙은 단어에서 어간이 접두사로 노출되어야 한다 (회귀 방지 핵심).
        let tokens = tokenize("해커톤에서 프로젝트를 만들었다");
        assert!(tokens.contains(&"해커톤".to_string()), "어간 '해커톤' 누락: {tokens:?}");
        assert!(tokens.contains(&"프로젝트".to_string()), "어간 '프로젝트' 누락: {tokens:?}");
        // 원본 전체 토큰도 보존되어야 한다.
        assert!(tokens.contains(&"해커톤에서".to_string()));
    }

    #[test]
    fn test_tokenize_english_extracted_from_mixed() {
        // 영문+조사 혼합에서 순수 영문 토큰이 분리되어야 한다.
        let tokens = tokenize("Rust와 React를 배웠다");
        assert!(tokens.contains(&"rust".to_string()), "영문 'rust' 누락: {tokens:?}");
        assert!(tokens.contains(&"react".to_string()), "영문 'react' 누락: {tokens:?}");
    }

    #[test]
    fn test_bm25_mixed_query_matches() {
        // 영/한 혼합 문서를 영문 어간 질의로 검색 시 매칭되어야 한다.
        let mut scorer = BM25Scorer::new();
        scorer.add_document("doc1", "React로 프론트엔드를 개발했다");
        scorer.add_document("doc2", "오늘 점심은 김치찌개였다");

        let scores = scorer.score("react 개발");
        let doc1_score = scores.iter().find(|(id, _)| id == "doc1").map(|(_, s)| *s).unwrap_or(0.0);
        let doc2_score = scores.iter().find(|(id, _)| id == "doc2").map(|(_, s)| *s).unwrap_or(0.0);
        assert!(doc1_score > doc2_score, "doc1={doc1_score} doc2={doc2_score}");
    }

    #[test]
    fn test_bm25_scoring() {
        let mut scorer = BM25Scorer::new();
        scorer.add_document("doc1", "해커톤에서 프로젝트를 만들었다");
        scorer.add_document("doc2", "오늘 날씨가 좋다");
        scorer.add_document("doc3", "해커톤 프로젝트 발표");

        let scores = scorer.score("해커톤 프로젝트");
        assert!(!scores.is_empty());
        // doc1이나 doc3이 doc2보다 높은 점수를 받아야 함
        let doc2_score = scores.iter().find(|(id, _)| id == "doc2").map(|(_, s)| *s).unwrap_or(0.0);
        let doc1_score = scores.iter().find(|(id, _)| id == "doc1").map(|(_, s)| *s).unwrap_or(0.0);
        assert!(doc1_score > doc2_score);
    }

    #[test]
    fn test_hangul_prefix_expansion_is_capped() {
        // 회귀 방지 (Cycle 2 blocking): 공백 없는 긴 한글 런이 접두사를 O(N²)로
        // 생성해 단일 요청 OOM을 유발하던 결함을 고정한다. 접두사 개수·토큰 수는
        // 런 길이 N과 무관하게 상한 이하로 유지되어야 한다.
        let n = 4096;
        let long_run: String = "가".repeat(n);
        let tokens = tokenize(&long_run);

        // 접두사(길이 ≤ 상한)는 최대 (상한 - MIN_TOKEN_GRAPHEMES + 1)개.
        let prefix_count = tokens
            .iter()
            .filter(|t| t.chars().count() <= MAX_HANGUL_PREFIX_LEN)
            .count();
        assert!(
            prefix_count <= MAX_HANGUL_PREFIX_LEN,
            "접두사가 상한되지 않음: {prefix_count}개 (N={n})"
        );

        // 전체 토큰 수도 입력 길이에 비례하지 않는다. (상한 미적용 시 ~N개가 되어 실패)
        assert!(
            tokens.len() <= MAX_HANGUL_PREFIX_LEN + 1,
            "토큰 수가 입력에 비례: {}개 (N={n})",
            tokens.len()
        );

        // 접두사들의 총 그래핌 길이(메모리 프록시)가 O(N²)가 아니라 O(상한²)여야 한다.
        let prefix_grapheme_sum: usize = tokens
            .iter()
            .map(|t| t.chars().count())
            .filter(|&len| len <= MAX_HANGUL_PREFIX_LEN)
            .sum();
        assert!(
            prefix_grapheme_sum <= MAX_HANGUL_PREFIX_LEN * MAX_HANGUL_PREFIX_LEN,
            "접두사 총 길이가 O(N²): {prefix_grapheme_sum}"
        );
    }

    #[test]
    fn test_tokenize_alternating_script_is_bounded() {
        // 회귀 방지: 스크립트가 교대되는 긴 무공백 입력("가가x나나x…")이
        // push_unique의 선형 dedup을 단어당 O(k²)로 악화시키던 잠재적 CPU DoS를
        // 고정한다. 단어 하나의 토큰 수는 MAX_TOKENS_PER_WORD로 상한된다.
        let mut s = String::new();
        for i in 0..4000u32 {
            // i마다 서로 다른 한글 음절 2개로 distinct한 2음절 런을 만든다(dedup 무력화).
            let c = char::from_u32(0xAC00 + i).unwrap();
            s.push(c);
            s.push(c);
            s.push('x'); // 라틴 문자 — 영숫자라 단어를 나누지 않고 스크립트만 교대시킨다.
        }
        let tokens = tokenize(&s);
        // 전체가 하나의 무공백 단어이므로 토큰 수는 단어 상한 이하여야 한다.
        // (상한 미적용 시 ~4000개가 되어 실패)
        assert!(
            tokens.len() <= MAX_TOKENS_PER_WORD,
            "교대 스크립트 단어의 토큰이 상한되지 않음: {}개",
            tokens.len()
        );
    }

    #[test]
    fn test_hangul_prefix_cap_preserves_realistic_recall() {
        // 상한 도입이 현실적 길이의 한국어 어간 노출을 해치지 않음을 고정한다.
        // 상한(32) 이내 복합명사+조사는 이전과 동일하게 모든 접두사가 생성된다.
        let tokens = tokenize("국제무역위원회에서");
        assert!(tokens.contains(&"국제".to_string()), "어간 접두사 누락: {tokens:?}");
        assert!(
            tokens.contains(&"국제무역위원회".to_string()),
            "조사 제거 어간 누락: {tokens:?}"
        );
        assert!(
            tokens.contains(&"국제무역위원회에서".to_string()),
            "원본 전체 토큰 누락: {tokens:?}"
        );
    }
}
