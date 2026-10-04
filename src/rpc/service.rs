//! gRPC 服务方法与阻塞编排操作调度。

use super::conversion::{edit_from_proto, stack_to_proto, status_from_error};
use super::proto;
use crate::config::Config;
use crate::orchestrator::{ApplyRequest, Asset, FilesUpload, Orchestrator};
use crate::template::{self, GeneratedFile};
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

    /// 不占用变更锁执行只读管理器操作。
    async fn reading<T, F>(&self, operation: F) -> Result<T, Status>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Orchestrator>) -> anyhow::Result<T> + Send + 'static,
    {
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || operation(manager))
            .await
            .map_err(|error| Status::internal(format!("后台查询失败: {error}")))?
            .map_err(|error| status_from_error(&error))
    }

    /// 创建依次报告排队、执行与完成阶段的变更操作流。
    fn operation_stream<F>(
        &self,
        running: String,
        operation: F,
    ) -> RpcStream<proto::OperationProgress>
    where
        F: FnOnce(Arc<Orchestrator>) -> anyhow::Result<String> + Send + 'static,
    {
        let manager = Arc::clone(&self.manager);
        let lock = Arc::clone(&self.lock);
        let (sender, receiver) = mpsc::channel(16);
        tokio::spawn(async move {
            let _guard = match lock.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    if send_operation_progress(
                        &sender,
                        proto::OperationStage::Queued,
                        "等待其他变更操作完成",
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                    lock.lock().await
                }
            };
            if send_operation_progress(&sender, proto::OperationStage::Running, &running)
                .await
                .is_err()
            {
                return;
            }
            let mut task = tokio::task::spawn_blocking(move || operation(manager));
            let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(5));
            heartbeat.tick().await;
            let mut elapsed = 0_u64;
            let result = loop {
                tokio::select! {
                    result = &mut task => break result,
                    _ = heartbeat.tick() => {
                        elapsed += 5;
                        let message = format!("{running}（已执行 {elapsed} 秒）");
                        let _send_result = send_operation_progress(
                            &sender,
                            proto::OperationStage::Running,
                            &message,
                        ).await;
                    }
                }
            };
            match result {
                Ok(Ok(message)) => {
                    let _send_result = send_operation_progress(
                        &sender,
                        proto::OperationStage::Completed,
                        &message,
                    )
                    .await;
                }
                Ok(Err(error)) => {
                    let _send_result = sender.send(Err(status_from_error(&error))).await;
                }
                Err(error) => {
                    let _send_result = sender
                        .send(Err(Status::internal(format!("后台任务失败: {error}"))))
                        .await;
                }
            }
        });
        Box::pin(ReceiverStream::new(receiver))
    }
}

/// 拉取镜像与日志 RPC 使用的流类型。
type RpcStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

/// 向仍连接的客户端发送一个修改操作阶段。
async fn send_operation_progress(
    sender: &mpsc::Sender<Result<proto::OperationProgress, Status>>,
    stage: proto::OperationStage,
    message: &str,
) -> Result<(), mpsc::error::SendError<Result<proto::OperationProgress, Status>>> {
    sender
        .send(Ok(proto::OperationProgress {
            stage: stage as i32,
            message: message.to_string(),
        }))
        .await
}

#[tonic::async_trait]
impl OrchestratorRpc for RpcService {
    async fn status(
        &self,
        _request: Request<proto::StatusRequest>,
    ) -> Result<Response<proto::StatusResponse>, Status> {
        let status = self
            .reading(|manager| {
                let config = manager.config();
                Ok(proto::StatusResponse {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    docker_available: crate::docker::available(config),
                    stacks_root: config.stacks_root.display().to_string(),
                    domain: config.domain.clone(),
                    listen: config.listen.clone(),
                })
            })
            .await?;
        Ok(Response::new(status))
    }

    type ApplyStream = RpcStream<proto::OperationProgress>;

    async fn apply(
        &self,
        request: Request<proto::ApplyRequest>,
    ) -> Result<Response<Self::ApplyStream>, Status> {
        let request = request.into_inner();
        Ok(Response::new(self.operation_stream(
            String::from("正在校验并应用项目配置"),
            move |manager| {
                let assets: Vec<Asset> = request
                    .assets
                    .iter()
                    .map(|asset| Asset {
                        path: PathBuf::from(&asset.path),
                        content: asset.content.clone(),
                    })
                    .collect();
                let project_files = request
                    .project_files
                    .iter()
                    .map(|asset| FilesUpload {
                        path: PathBuf::from(&asset.path),
                        file: GeneratedFile {
                            path: PathBuf::from(template::files::FILES_DIRECTORY).join(&asset.path),
                            content: asset.content.clone(),
                            mode: 0o644,
                            replace: true,
                        },
                    })
                    .collect::<Vec<_>>();
                let files_into = if request.files_into.is_empty() {
                    String::from(template::files::DEFAULT_FILES_TARGET)
                } else {
                    request.files_into.clone()
                };
                manager.apply(&ApplyRequest {
                    config_toml: &request.config_toml,
                    assets: &assets,
                    assets_provided: request.assets_provided || !assets.is_empty(),
                    replace_assets: request.assets_mode == proto::AssetsMode::Replace as i32,
                    project_files: &project_files,
                    files_into: &files_into,
                    force: request.force,
                    start: request.start,
                    restart_dependents: request.restart_dependents,
                })
            },
        )))
    }

    type ImportComposeStream = RpcStream<proto::OperationProgress>;

    async fn import_compose(
        &self,
        request: Request<proto::ImportComposeRequest>,
    ) -> Result<Response<Self::ImportComposeStream>, Status> {
        let request = request.into_inner();
        Ok(Response::new(self.operation_stream(
            format!("正在导入项目 {}", request.name),
            move |manager| {
                manager.import_compose(
                    &request.name,
                    &request.compose_yaml,
                    request.env_file.as_deref(),
                    request.start,
                )
            },
        )))
    }

    async fn export(
        &self,
        request: Request<proto::ExportRequest>,
    ) -> Result<Response<proto::ExportResponse>, Status> {
        let request = request.into_inner();
        let name = request.name;
        let keep_comments = request.keep_comments;
        let config_toml = self
            .reading(move |manager| manager.export(&name, keep_comments))
            .await?;
        Ok(Response::new(proto::ExportResponse { config_toml }))
    }

    async fn doctor(
        &self,
        _request: Request<proto::DoctorRequest>,
    ) -> Result<Response<proto::DoctorResponse>, Status> {
        let report = self
            .reading(|manager| crate::doctor::report(manager.config()))
            .await?;
        Ok(Response::new(proto::DoctorResponse {
            report: report.text,
            problems: report.problems,
        }))
    }

    async fn set_domain(
        &self,
        request: Request<proto::SetDomainRequest>,
    ) -> Result<Response<proto::SetDomainResponse>, Status> {
        let domain = request.into_inner().domain;
        let requested = domain.clone();
        let affected = self
            .reading(move |manager| manager.set_domain(&requested))
            .await?;
        let mut message = format!("主域名已更新为 {domain}；重启 daemon 后对新请求生效");
        if !affected.is_empty() {
            message.push_str(&format!(
                "。以下项目仍钉定了其它域名，需要手工调整：{}",
                affected.join(", ")
            ));
        }
        Ok(Response::new(proto::SetDomainResponse {
            message,
            restart_required: true,
        }))
    }

    type EditStream = RpcStream<proto::OperationProgress>;

    async fn edit(
        &self,
        request: Request<proto::EditRequest>,
    ) -> Result<Response<Self::EditStream>, Status> {
        let request = request.into_inner();
        let name = request.name.clone();
        let edit = edit_from_proto(request).map_err(|error| status_from_error(&error))?;
        Ok(Response::new(self.operation_stream(
            format!("正在修改项目 {name}"),
            move |manager| manager.edit(&name, edit),
        )))
    }

    async fn list(
        &self,
        _request: Request<proto::ListRequest>,
    ) -> Result<Response<proto::ListResponse>, Status> {
        let stacks = self.reading(move |manager| manager.list()).await?;
        Ok(Response::new(proto::ListResponse {
            stacks: stacks.into_iter().map(stack_to_proto).collect(),
        }))
    }

    async fn get(
        &self,
        request: Request<proto::GetRequest>,
    ) -> Result<Response<proto::Stack>, Status> {
        let name = request.into_inner().name;
        let stack = self.reading(move |manager| manager.get(&name)).await?;
        Ok(Response::new(stack_to_proto(stack)))
    }

    type RemoveStream = RpcStream<proto::OperationProgress>;

    async fn remove(
        &self,
        request: Request<proto::RemoveRequest>,
    ) -> Result<Response<Self::RemoveStream>, Status> {
        let request = request.into_inner();
        Ok(Response::new(self.operation_stream(
            format!("正在删除项目 {}", request.name),
            move |manager| manager.remove(&request.name, request.force),
        )))
    }

    type StartStream = RpcStream<proto::OperationProgress>;

    async fn start(
        &self,
        request: Request<proto::ActionRequest>,
    ) -> Result<Response<Self::StartStream>, Status> {
        let name = request.into_inner().name;
        Ok(Response::new(self.operation_stream(
            format!("正在启动项目 {name}"),
            move |manager| manager.start(&name),
        )))
    }

    type StopStream = RpcStream<proto::OperationProgress>;

    async fn stop(
        &self,
        request: Request<proto::ActionRequest>,
    ) -> Result<Response<Self::StopStream>, Status> {
        let name = request.into_inner().name;
        Ok(Response::new(self.operation_stream(
            format!("正在停止项目 {name}"),
            move |manager| manager.stop(&name),
        )))
    }

    type RestartStream = RpcStream<proto::OperationProgress>;

    async fn restart(
        &self,
        request: Request<proto::ActionRequest>,
    ) -> Result<Response<Self::RestartStream>, Status> {
        let name = request.into_inner().name;
        Ok(Response::new(self.operation_stream(
            format!("正在重启项目 {name}"),
            move |manager| manager.restart(&name),
        )))
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
                let connection = sender.clone();
                manager.pull(
                    &name,
                    |progress| {
                        sender
                            .blocking_send(Ok(proto::PullProgress {
                                id: progress.id,
                                status: progress.status,
                                text: progress.text,
                                current: progress.current,
                                total: progress.total,
                            }))
                            .is_ok()
                    },
                    || !connection.is_closed(),
                )
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

    type BuildStream = RpcStream<proto::OperationProgress>;

    async fn build(
        &self,
        request: Request<proto::ActionRequest>,
    ) -> Result<Response<Self::BuildStream>, Status> {
        let name = request.into_inner().name;
        Ok(Response::new(self.operation_stream(
            format!("正在构建项目 {name}"),
            move |manager| manager.build(&name),
        )))
    }

    type LogsStream = RpcStream<proto::LogLine>;

    async fn logs(
        &self,
        request: Request<proto::LogsRequest>,
    ) -> Result<Response<Self::LogsStream>, Status> {
        let request = request.into_inner();
        let manager = Arc::clone(&self.manager);
        let (sender, receiver) = mpsc::channel(128);
        let error_sender = sender.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let connection = sender.clone();
                manager.logs(
                    &request.name,
                    request.tail,
                    request.follow,
                    |line| sender.blocking_send(Ok(proto::LogLine { line })).is_ok(),
                    || !connection.is_closed(),
                )
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
