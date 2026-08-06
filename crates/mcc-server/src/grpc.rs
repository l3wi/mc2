//! Agent gRPC service (`mcc.agent.v1.AgentService`).

use mcc_api::agent::agent_service_server::AgentService;
use mcc_api::agent::{
    HeartbeatRequest, HeartbeatResponse, JoinRequest, JoinResponse, ReportStatusRequest,
    ReportStatusResponse, SyncRequest, SyncResponse,
};
use mcc_store::{hash_token, NodeHeartbeat, NodeJoin, Store};
use rand::RngCore;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

/// gRPC handlers backed by the shared [`Store`].
#[derive(Clone)]
pub struct AgentSvc {
    pub store: Arc<dyn Store>,
}

fn gen_node_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("mccnt_{}", hex::encode(bytes))
}

#[tonic::async_trait]
impl AgentService for AgentSvc {
    async fn join(&self, request: Request<JoinRequest>) -> Result<Response<JoinResponse>, Status> {
        let req = request.into_inner();
        if req.node_name.trim().is_empty() {
            return Err(Status::invalid_argument("node_name is required"));
        }
        if !self
            .store
            .verify_join_token(&req.join_token)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
        {
            return Err(Status::unauthenticated("invalid join token"));
        }

        let cap = req.capacity.unwrap_or_default();
        let labels_json = serde_json::to_string(&req.labels).unwrap_or_else(|_| "{}".into());
        let node_token = gen_node_token();

        let rec = self
            .store
            .upsert_node_join(NodeJoin {
                name: req.node_name.clone(),
                labels_json,
                arch: req.arch,
                cpus: cap.cpus,
                memory_mib: cap.memory_mib,
                node_token_hash: hash_token(&node_token),
            })
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        info!(node_id = %rec.id, name = %rec.name, "node joined");

        Ok(Response::new(JoinResponse {
            node_id: rec.id,
            node_token,
        }))
    }

    async fn heartbeat(
        &self,
        request: Request<HeartbeatRequest>,
    ) -> Result<Response<HeartbeatResponse>, Status> {
        let req = request.into_inner();
        let cap = req.capacity.unwrap_or_default();
        let status = if req.status.is_empty() {
            "Ready".to_string()
        } else {
            req.status
        };

        match self
            .store
            .heartbeat_node(
                &req.node_id,
                &req.node_token,
                NodeHeartbeat {
                    cpus: cap.cpus,
                    memory_mib: cap.memory_mib,
                    status,
                },
            )
            .await
        {
            Ok(_) => Ok(Response::new(HeartbeatResponse { ok: true })),
            Err(mcc_store::StoreError::Unauthorized) => {
                Err(Status::unauthenticated("invalid node token"))
            }
            Err(mcc_store::StoreError::NotFound(_)) => Err(Status::not_found("unknown node")),
            Err(e) => {
                warn!(error = %e, "heartbeat failed");
                Err(Status::internal(e.to_string()))
            }
        }
    }

    async fn sync(&self, request: Request<SyncRequest>) -> Result<Response<SyncResponse>, Status> {
        let req = request.into_inner();
        // Auth check for future use; return empty desired set until Phase 3.
        let node = self
            .store
            .get_node(&req.node_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("unknown node"))?;
        if !mcc_store::verify_token(&req.node_token, &node.node_token_hash) {
            return Err(Status::unauthenticated("invalid node token"));
        }
        Ok(Response::new(SyncResponse { instances: vec![] }))
    }

    async fn report_status(
        &self,
        request: Request<ReportStatusRequest>,
    ) -> Result<Response<ReportStatusResponse>, Status> {
        let req = request.into_inner();
        let node = self
            .store
            .get_node(&req.node_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("unknown node"))?;
        if !mcc_store::verify_token(&req.node_token, &node.node_token_hash) {
            return Err(Status::unauthenticated("invalid node token"));
        }
        // Phase 3 will persist instance status.
        Ok(Response::new(ReportStatusResponse { ok: true }))
    }
}
