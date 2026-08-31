//! gRPC 服务方法与阻塞编排操作调度。

use super::conversion::{edit_from_proto, operation_response, stack_to_proto, status_from_error};
use super::proto;
use crate::config::Config;
use crate::orchestrator::{Asset, Orchestrator};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use proto::orchestrator_server::Orchestrator as OrchestratorRpc;

/// 在进程范围内串行执行操作的守护进程 RPC 服务。
#[derive(Debug, Clone)]
pub struct RpcService {
    /// 共享项目管理器。
    manager: Arc<Orchestrator>,
    /// 全局变更与 Docker 操作锁。
    lock: Arc<Mutex<()>>,
}

impl RpcService {
    /// 根据一份守护进程配置创建 RPC 服务。
    ///
    /// # 错误
    ///
    /// 管理器配置无效时返回错误。
    pub fn new(config: Config) -> anyhow::Result<Self> {
        Ok(Self {
            manager: Arc::new(Orchestrator::new(config)?),
            lock: Arc::new(Mutex::new(())),
        })
    }

    /// 持有全局锁时执行一次阻塞式管理器操作。
    async fn blocking<T, F>(&self, operation: F) -> Result<T, Status>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Orchestrator>) -> anyhow::Result<T> + Send + 'static,
    {
        let _guard = self.lock.lock().await;
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || operation(manager))
            .await
            .map_err(|error| Status::internal(format!("后台任务失败: {error}")))?
            .map_err(|error| status_from_error(&error))
    }
}

/// 拉取镜像与日志 RPC 使用的流类型。
type RpcStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl OrchestratorRpc for RpcService {
    async fn status(
        &self,
        _request: Request<proto::StatusRequest>,
    ) -> Result<Response<proto::StatusResponse>, Status> {
        let status = self
            .blocking(|manager| {
                let config = manager.config();
                Ok(proto::StatusResponse {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    docker_available: crate::docker::available(config),
                    stacks_root: config.stacks_root.display().to_string(),
                    domain: config.domain.clone(),
                })
            })
            .await?;
        Ok(Response::new(status))
    }

    async fn apply(
        &self,
        request: Request<proto::ApplyRequest>,
    ) -> Result<Response<proto::OperationResponse>, Status> {
        let request = request.into_inner();
        let result = self
            .blocking(move |manager| {
                manager.apply(
                    &request.config_toml,
                    request
                        .assets
                        .into_iter()
                        .map(|asset| Asset {
                            path: PathBuf::from(asset.path),
                            content: asset.content,
                        })
                        .collect(),
                    request.force,
                    request.start,
                )
            })
            .await?;
        Ok(operation_response(result))
    }

    async fn import_compose(
        &self,
        request: Request<proto::ImportComposeRequest>,
    ) -> Result<Response<proto::OperationResponse>, Status> {
        let request = request.into_inner();
        let result = self
            .blocking(move |manager| {
                manager.import_compose(
                    &request.name,
                    &request.compose_yaml,
                    request.env_file.as_deref(),
                    request.start,
                )
            })
            .await?;
        Ok(operation_response(result))
    }

    async fn export(
        &self,
        request: Request<proto::ExportRequest>,
    ) -> Result<Response<proto::ExportResponse>, Status> {
        let name = request.into_inner().name;
        let config_toml = self.blocking(move |manager| manager.export(&name)).await?;
        Ok(Response::new(proto::ExportResponse { config_toml }))
    }

    async fn edit(
        &self,
        request: Request<proto::EditRequest>,
    ) -> Result<Response<proto::OperationResponse>, Status> {
        let request = request.into_inner();
        let name = request.name.clone();
        let edit = edit_from_proto(request).map_err(|error| status_from_error(&error))?;
        let message = self
            .blocking(move |manager| manager.edit(&name, edit))
            .await?;
        Ok(operation_response(message))
    }

    async fn list(
        &self,
        _request: Request<proto::ListRequest>,
    ) -> Result<Response<proto::ListResponse>, Status> {
        let stacks = self.blocking(move |manager| manager.list()).await?;
        Ok(Response::new(proto::ListResponse {
            stacks: stacks.into_iter().map(stack_to_proto).collect(),
        }))
    }

    async fn get(
        &self,
        request: Request<proto::GetRequest>,
    ) -> Result<Response<proto::Stack>, Status> {
        let name = request.into_inner().name;
        let stack = self.blocking(move |manager| manager.get(&name)).await?;
        Ok(Response::new(stack_to_proto(stack)))
    }

    async fn remove(
        &self,
        request: Request<proto::RemoveRequest>,
    ) -> Result<Response<proto::OperationResponse>, Status> {
        let request = request.into_inner();
        let message = self
            .blocking(move |manager| manager.remove(&request.name, request.force))
            .await?;
        Ok(operation_response(message))
    }

    async fn start(
        &self,
        request: Request<proto::ActionRequest>,
    ) -> Result<Response<proto::OperationResponse>, Status> {
        let name = request.into_inner().name;
        let message = self.blocking(move |manager| manager.start(&name)).await?;
        Ok(operation_response(message))
    }

    async fn stop(
        &self,
        request: Request<proto::ActionRequest>,
    ) -> Result<Response<proto::OperationResponse>, Status> {
        let name = request.into_inner().name;
        let message = self.blocking(move |manager| manager.stop(&name)).await?;
        Ok(operation_response(message))
    }

    async fn restart(
        &self,
        request: Request<proto::ActionRequest>,
    ) -> Result<Response<proto::OperationResponse>, Status> {
        let name = request.into_inner().name;
        let message = self.blocking(move |manager| manager.restart(&name)).await?;
        Ok(operation_response(message))
    }

    type PullStream = RpcStream<proto::PullProgress>;

    async fn pull(
        &self,
        request: Request<proto::ActionRequest>,
    ) -> Result<Response<Self::PullStream>, Status> {
        let name = request.into_inner().name;
        let manager = Arc::clone(&self.manager);
        let lock = Arc::clone(&self.lock);
        let (sender, receiver) = mpsc::channel(32);
        let error_sender = sender.clone();
        tokio::spawn(async move {
            let _guard = lock.lock().await;
            let result = tokio::task::spawn_blocking(move || {
                manager.pull(&name, |progress| {
                    sender
                        .blocking_send(Ok(proto::PullProgress {
                            id: progress.id,
                            status: progress.status,
                            text: progress.text,
                            current: progress.current,
                            total: progress.total,
                        }))
                        .is_ok()
                })
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let status = status_from_error(&error);
                    let _send_result = error_sender.send(Err(status)).await;
                }
                Err(error) => {
                    let status = Status::internal(format!("镜像拉取后台任务失败: {error}"));
                    let _send_result = error_sender.send(Err(status)).await;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }

    async fn build(
        &self,
        request: Request<proto::ActionRequest>,
    ) -> Result<Response<proto::OperationResponse>, Status> {
        let name = request.into_inner().name;
        let message = self.blocking(move |manager| manager.build(&name)).await?;
        Ok(operation_response(message))
    }

    type LogsStream = RpcStream<proto::LogLine>;

    async fn logs(
        &self,
        request: Request<proto::LogsRequest>,
    ) -> Result<Response<Self::LogsStream>, Status> {
        let request = request.into_inner();
        let manager = Arc::clone(&self.manager);
        let lock = Arc::clone(&self.lock);
        let (sender, receiver) = mpsc::channel(128);
        let error_sender = sender.clone();
        tokio::spawn(async move {
            let _guard = lock.lock().await;
            let result = tokio::task::spawn_blocking(move || {
                manager.logs(&request.name, request.tail, request.follow, |line| {
                    sender.blocking_send(Ok(proto::LogLine { line })).is_ok()
                })
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let status = status_from_error(&error);
                    let _send_result = error_sender.send(Err(status)).await;
                }
                Err(error) => {
                    let status = Status::internal(format!("日志后台任务失败: {error}"));
                    let _send_result = error_sender.send(Err(status)).await;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}
