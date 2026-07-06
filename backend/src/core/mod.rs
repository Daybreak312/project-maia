mod indexer;
mod search;
mod ingest_agent;
mod search_agent;

pub use indexer::{cross_workspace_targets, Indexer, TimeSearchOptions};
pub use search::{BM25Scorer, SearchMode, reciprocal_rank_fusion, tokenize};
// 외부(api)에서 deep search 파라미터를 구성하는 데 필요한 타입만 노출한다.
// SearchAgent/SearchBackend/ExpandOrigin 등 파이프라인 내부 타입은 indexer가
// search_agent 경로로 직접 접근하므로 재export하지 않는다.
pub use search_agent::{DeepSearchParams, DEFAULT_DEEP_SEARCH_MAX_RESULTS};
