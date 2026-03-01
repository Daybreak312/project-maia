use std::collections::{HashMap, HashSet};
use unicode_segmentation::UnicodeSegmentation;

/// BM25 파라미터
const K1: f32 = 1.2;
const B: f32 = 0.75;

/// 키워드 검색을 위한 토큰화 (한국어 + 영어 지원)
pub fn tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();

    // 한국어는 음절 단위, 영어는 단어 단위로 분리
    let mut tokens = Vec::new();
    let mut current_word = String::new();

    for c in lower.chars() {
        if c.is_alphanumeric() {
            current_word.push(c);
        } else if !current_word.is_empty() {
            tokens.push(std::mem::take(&mut current_word));
        }
    }

    if !current_word.is_empty() {
        tokens.push(current_word);
    }

    // 2글자 이상인 토큰만 유지
    tokens.into_iter().filter(|t| t.graphemes(true).count() >= 2).collect()
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
}
