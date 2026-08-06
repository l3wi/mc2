//! Agent gRPC service (`mcc.agent.v1.AgentService`).

use crate::secrets::resolve_injections;
use mcc_api::agent::agent_service_server::AgentService;
use mcc_api::agent::{
    HeartbeatRequest, HeartbeatResponse, JoinRequest, JoinResponse, ReportStatusRequest,
    ReportStatusResponse, SecretInjection, SyncRequest, SyncResponse,
};
use mcc_api::ServiceSpec;
use mcc_store::{hash_token, NodeHeartbeat, NodeJoin, SecretsKey, Store};
use rand::RngCore;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

/// gRPC handlers backed by the shared [`Store`].
#[derive(Clone)]
pub struct AgentSvc {
    pub store: Arc<dyn Store>,
    pub secrets_key: Arc<SecretsKey>,
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
        let node = self
            .store
            .get_node(&req.node_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("unknown node"))?;
        if !mcc_store::verify_token(&req.node_token, &node.node_token_hash) {
            return Err(Status::unauthenticated("invalid node token"));
        }

        let rows = self
            .store
            .list_instances_for_node(&req.node_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        // Include all phases bound to this node so restartPolicy can act on
        // Failed/Stopped (scale-down deletes rows; unbound instances leave Sync).
        let mut instances = Vec::new();
        for i in rows {
            let spec: ServiceSpec = serde_json::from_str(&i.spec_json)
                .map_err(|e| Status::internal(format!("parse service spec for {}: {e}", i.id)))?;
            let resolved =
                resolve_injections(self.store.clone(), self.secrets_key.as_ref(), &spec.secrets)
                    .await
                    .map_err(|e| Status::failed_precondition(e.to_string()))?;

            let secrets = resolved
                .into_iter()
                .map(|s| SecretInjection {
                    env: s.env,
                    value: s.value,
                    allow_hosts: s.allow_hosts,
                })
                .collect();

            instances.push(mcc_api::agent::DesiredInstance {
                instance_id: i.id,
                stack: i.stack,
                service: i.service,
                ordinal: i.ordinal,
                spec_json: i.spec_json,
                secrets,
            });
        }

        Ok(Response::new(SyncResponse { instances }))
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

        for st in req.instances {
            if let Some(inst) = self
                .store
                .get_instance(&st.instance_id)
                .await
                .map_err(|e| Status::internal(e.to_string()))?
            {
                if let Some(ref nid) = inst.node_id {
                    if nid != &req.node_id {
                        continue;
                    }
                }
            }
            let _ = self
                .store
                .update_instance_status(
                    &st.instance_id,
                    &st.phase,
                    if st.runtime_id.is_empty() {
                        None
                    } else {
                        Some(st.runtime_id.as_str())
                    },
                    if st.message.is_empty() {
                        None
                    } else {
                        Some(st.message.as_str())
                    },
                )
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
        }

        Ok(Response::new(ReportStatusResponse { ok: true }))
    }
}
