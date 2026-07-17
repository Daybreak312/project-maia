use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

use crate::api::{require_write, resolve_and_authorize_workspace, WorkspaceQuery};
use crate::auth::AuthContext;
use crate::models::{Edge, RelationType};
use crate::storage::{NeighborNode, MAX_NEIGHBOR_DEPTH};
use crate::AppState;

/// 수동 엣지의 기본 가중치. 사람이 명시적으로 만든 관계라 확신도를 높게(1.0) 둔다
/// (자동 엣지의 DEFAULT_EDGE_WEIGHT=0.5와 대비).
const MANUAL_EDGE_WEIGHT: f32 = 1.0;

// ──────────────────────────────────────────────────────────────
// 이웃 조회: GET /documents/:id/neighbors
// ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct NeighborQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// 탐색 깊이 (미지정 시 1). 서버가 [1, MAX_NEIGHBOR_DEPTH]로 클램프한다.
    #[serde(default)]
    pub depth: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct NeighborsResponse {
    pub start: Uuid,
    /// 실제 적용된(클램프된) 탐색 깊이
    pub depth: usize,
    pub neighbors: Vec<NeighborView>,
}

#[derive(Debug, Serialize)]
pub struct NeighborView {
    pub id: Uuid,
    pub summary: String,
    /// 시작 문서로부터의 거리 (1 = 직접 이웃)
    pub depth: usize,
    /// 직전 문서에서 이 문서로 향한 관계 타입
    pub relation: String,
    /// 직전(부모) 문서 ID
    pub via: Uuid,
    pub weight: f32,
}

impl From<NeighborNode> for NeighborView {
    fn from(n: NeighborNode) -> Self {
        Self {
            id: n.document.id,
            summary: n.document.summary,
            depth: n.depth,
            relation: n.relation.as_str().to_string(),
            via: n.via,
            weight: n.weight,
        }
    }
}

pub async fn neighbors_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
    Query(q): Query<NeighborQuery>,
) -> Result<Json<NeighborsResponse>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, q.workspace).await?;
    let requested_depth = q.depth.unwrap_or(1);
    let effective_depth = requested_depth.clamp(1, MAX_NEIGHBOR_DEPTH);

    let nodes = state
        .indexer
        .neighbors_in_workspace(id, requested_depth, &workspace)
        .await
        .map_err(|e| {
            tracing::error!("Neighbors query failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    let neighbors = nodes.into_iter().map(NeighborView::from).collect();

    Ok(Json(NeighborsResponse {
        start: id,
        depth: effective_depth,
        neighbors,
    }))
}

// ──────────────────────────────────────────────────────────────
// 수동 엣지 추가: POST /documents/:id/edges
// ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddEdgeRequest {
    /// 대상 문서 ID
    pub target: Uuid,
    /// 관계 타입 (related_to/updates/contradicts/references/part_of)
    pub relation: String,
    /// 가중치 (0~1). 미지정 시 수동 기본값.
    #[serde(default)]
    pub weight: Option<f32>,
}

#[derive(Debug, Serialize)]
pub struct EdgeMutationResponse {
    pub source: Uuid,
    /// 변경 후 이 문서의 총 엣지 수
    pub edge_count: usize,
}

pub async fn add_edge_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
    Query(wq): Query<WorkspaceQuery>,
    Json(req): Json<AddEdgeRequest>,
) -> Result<Json<EdgeMutationResponse>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    require_write(&ctx, &workspace)?;

    let relation = RelationType::from_str(&req.relation).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            format!("Unknown relation type: '{}'", req.relation),
        )
    })?;

    let weight = req.weight.unwrap_or(MANUAL_EDGE_WEIGHT);
    let edge = Edge::new(req.target, relation, weight);

    state
        .indexer
        .add_edge_to_document(&workspace, id, edge)
        .await
        .map(|doc| {
            Json(EdgeMutationResponse {
                source: id,
                edge_count: doc.edges.len(),
            })
        })
        .map_err(|e| {
            tracing::error!("Add edge failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}

// ──────────────────────────────────────────────────────────────
// 수동 엣지 제거: DELETE /documents/:id/edges/:target
// ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct RemoveEdgeResponse {
    pub source: Uuid,
    pub target: Uuid,
    /// 제거된 엣지 수 (같은 target의 서로 다른 relation이 여럿이면 2 이상)
    pub removed: usize,
}

pub async fn remove_edge_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path((id, target)): Path<(Uuid, Uuid)>,
    Query(wq): Query<WorkspaceQuery>,
) -> Result<Json<RemoveEdgeResponse>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    require_write(&ctx, &workspace)?;

    state
        .indexer
        .remove_edge_from_document(&workspace, id, target)
        .await
        .map(|removed| {
            Json(RemoveEdgeResponse {
                source: id,
                target,
                removed,
            })
        })
        .map_err(|e| {
            tracing::error!("Remove edge failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Document, RelationType};

    #[test]
    fn test_neighbor_view_from_node() {
        // NeighborNode → NeighborView 변환이 필드를 정확히 옮겨야 한다.
        let mut doc = Document::new("c".into(), "요약".into(), vec![], vec![]);
        let via = Uuid::new_v4();
        doc.id = Uuid::new_v4();
        let doc_id = doc.id;
        let node = NeighborNode {
            document: doc,
            depth: 2,
            relation: RelationType::PartOf,
            via,
            weight: 0.5,
        };
        let view = NeighborView::from(node);
        assert_eq!(view.id, doc_id);
        assert_eq!(view.summary, "요약");
        assert_eq!(view.depth, 2);
        assert_eq!(view.relation, "part_of");
        assert_eq!(view.via, via);
    }
}
