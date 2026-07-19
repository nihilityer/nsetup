//! gRPC 编排服务实现。

use super::conversion::{
    application_spec, infrastructure_spec, invalid_generation, operation_response, stack_to_proto,
    static_site_spec, status_from_error,
};
use super::proto::orchestrator_server::Orchestrator;
use super::proto::{
    CreateApplicationRequest, CreateStaticSiteRequest, DeployStackRequest, GetLogsRequest,
    GetStackRequest, HealthRequest, HealthResponse, InitializeInfrastructureRequest,
    ListStacksRequest, ListStacksResponse, LogLine, OperationResponse, PullProgress,
    RemoveStackRequest, Stack, StackActionRequest,
};
use crate::config::{Config, check_docker};
use crate::generator::{self, Route};
use crate::orchestrator::{self};
use crate::services::docker;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

/// gRPC 编排服务状态。
#[derive(Debug, Clone)]
pub(super) struct OrchestratorService {
    /// daemon 加载的系统配置。
    config: Config,
    /// 串行化所有改变机器状态的操作。
    operations: Arc<Mutex<()>>,
}

impl OrchestratorService {
    /// 创建服务状态。
    pub(super) fn new(config: Config) -> Self {
        Self {
            config,
            operations: Arc::new(Mutex::new(())),
        }
    }

    /// 在线程池中执行阻塞的文件和 Docker 操作。
    async fn blocking<T, F>(&self, operation: F) -> Result<T, Status>
    where
        T: Send + 'static,
        F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    {
        tokio::task::spawn_blocking(operation)
            .await
            .map_err(|error| Status::internal(format!("后台任务执行失败: {error}")))?
            .map_err(|error| status_from_error(&error))
    }

    /// 串行执行会改变项目状态的操作。
    async fn mutate<T, F>(&self, operation: F) -> Result<T, Status>
    where
        T: Send + 'static,
        F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    {
        let _guard = self.operations.lock().await;
        self.blocking(operation).await
    }

    /// 执行通用的 Compose 生命周期操作。
    async fn stack_action<F>(
        &self,
        request: Request<StackActionRequest>,
        action: &'static str,
        operation: F,
    ) -> Result<Response<OperationResponse>, Status>
    where
        F: FnOnce(&Path) -> anyhow::Result<()> + Send + 'static,
    {
        let config = self.config.clone();
        let name = request.into_inner().name;
        let response_name = name.clone();
        self.mutate(move || {
            let directory = orchestrator::stack_dir(&config, &name)?;
            operation(&directory)
        })
        .await?;
        Ok(operation_response(format!(
            "项目 {response_name} 已{action}"
        )))
    }
}

#[tonic::async_trait]
impl Orchestrator for OrchestratorService {
    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        let docker_available = self.blocking(|| Ok(check_docker().is_ok())).await?;
        Ok(Response::new(HealthResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            docker_available,
            compose_root: self.config.paths.apps_root.display().to_string(),
        }))
    }
    async fn initialize_infrastructure(
        &self,
        request: Request<InitializeInfrastructureRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let config = self.config.clone();
        let input = request.into_inner();
        self.mutate(move || {
            let spec = infrastructure_spec(&input);
            let stacks = generator::generate_infrastructure(&spec, &config.paths.data_root)
                .map_err(invalid_generation)?;
            if !input.force {
                for stack in &stacks {
                    if orchestrator::stack_dir(&config, &stack.name)?.exists() {
                        return Err(orchestrator::InvalidInput(format!(
                            "项目 {} 已存在；确认覆盖请使用 --force",
                            stack.name
                        ))
                        .into());
                    }
                }
            }
            for stack in &stacks {
                orchestrator::deploy_generated_stack(&config, stack, input.force, input.start)?;
            }
            Ok(())
        })
        .await?;
        Ok(operation_response(String::from("Traefik 基础设施已生成")))
    }

    async fn create_application(
        &self,
        request: Request<CreateApplicationRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let config = self.config.clone();
        let input = request.into_inner();
        let name = input.name.clone();
        self.mutate(move || {
            let start = input.start;
            let force = input.force;
            let spec = application_spec(input)?;
            orchestrator::ensure_no_conflicts(
                &config,
                &spec.name,
                &spec.routes,
                &spec.published_ports,
            )?;
            let generated = generator::generate_application(&spec).map_err(invalid_generation)?;
            orchestrator::deploy_generated_stack(&config, &generated, force, false)?;
            if start {
                docker::compose_up(&orchestrator::stack_dir(&config, &spec.name)?)?;
            }
            Ok(())
        })
        .await?;
        Ok(operation_response(format!("应用 {name} 已生成")))
    }

    async fn create_static_site(
        &self,
        request: Request<CreateStaticSiteRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let config = self.config.clone();
        let input = request.into_inner();
        let name = input.name.clone();
        let start = input.start;
        let force = input.force;
        self.mutate(move || {
            let spec = static_site_spec(input)?;
            let routes = vec![Route {
                host: spec.host.clone(),
                path_prefix: None,
            }];
            orchestrator::ensure_no_conflicts(&config, &spec.name, &routes, &[])?;
            let generated = generator::generate_static_site(&spec).map_err(invalid_generation)?;
            orchestrator::deploy_generated_stack(&config, &generated, force, start)
        })
        .await?;
        Ok(operation_response(format!("静态站点 {name} 已生成")))
    }

    async fn list_stacks(
        &self,
        _request: Request<ListStacksRequest>,
    ) -> Result<Response<ListStacksResponse>, Status> {
        let config = self.config.clone();
        let stacks = self
            .blocking(move || orchestrator::list_stacks(&config))
            .await?
            .into_iter()
            .map(stack_to_proto)
            .collect();
        Ok(Response::new(ListStacksResponse { stacks }))
    }

    async fn get_stack(
        &self,
        request: Request<GetStackRequest>,
    ) -> Result<Response<Stack>, Status> {
        let config = self.config.clone();
        let name = request.into_inner().name;
        let stack = self
            .blocking(move || orchestrator::get_stack(&config, &name))
            .await?;
        Ok(Response::new(stack_to_proto(stack)))
    }

    async fn deploy_stack(
        &self,
        request: Request<DeployStackRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let config = self.config.clone();
        let input = request.into_inner();
        let name = input.name.clone();
        self.mutate(move || {
            orchestrator::deploy_stack(
                &config,
                &input.name,
                &input.compose_yaml,
                &input.env_file,
                input.start,
            )
        })
        .await?;
        Ok(operation_response(format!("项目 {name} 已部署")))
    }

    async fn remove_stack(
        &self,
        request: Request<RemoveStackRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let config = self.config.clone();
        let input = request.into_inner();
        let name = input.name.clone();
        self.mutate(move || orchestrator::remove_stack(&config, &input.name, input.remove_volumes))
            .await?;
        Ok(operation_response(format!("项目 {name} 已删除")))
    }

    async fn start_stack(
        &self,
        request: Request<StackActionRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        self.stack_action(request, "启动", docker::compose_up).await
    }

    async fn stop_stack(
        &self,
        request: Request<StackActionRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        self.stack_action(request, "停止", docker::compose_stop)
            .await
    }

    async fn restart_stack(
        &self,
        request: Request<StackActionRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        self.stack_action(request, "重启", docker::compose_restart)
            .await
    }

    /// 服务端流式镜像拉取事件。
    type PullStackStream = ReceiverStream<Result<PullProgress, Status>>;

    async fn pull_stack(
        &self,
        request: Request<StackActionRequest>,
    ) -> Result<Response<Self::PullStackStream>, Status> {
        let config = self.config.clone();
        let operations = self.operations.clone();
        let name = request.into_inner().name;
        let directory =
            orchestrator::stack_dir(&config, &name).map_err(|error| status_from_error(&error))?;
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            let _guard = operations.lock().await;
            let progress_sender = sender.clone();
            let result = tokio::task::spawn_blocking(move || {
                docker::compose_pull(
                    &directory,
                    |progress| {
                        progress_sender
                            .blocking_send(Ok(PullProgress {
                                id: progress.id,
                                status: progress.status,
                                text: progress.text,
                                current: progress.current,
                                total: progress.total,
                            }))
                            .is_ok()
                    },
                    || !progress_sender.is_closed(),
                )
            })
            .await;
            let error = match result {
                Ok(Err(error)) => Some(status_from_error(&error)),
                Err(error) => Some(Status::internal(format!("后台任务执行失败: {error}"))),
                Ok(Ok(())) => None,
            };
            if let Some(error) = error {
                let _result = sender.send(Err(error)).await;
            }
        });
        Ok(Response::new(ReceiverStream::new(receiver)))
    }

    async fn build_stack(
        &self,
        request: Request<StackActionRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        self.stack_action(request, "构建镜像", docker::compose_build)
            .await
    }

    /// 服务端流式 Compose 日志行。
    type GetLogsStream = ReceiverStream<Result<LogLine, Status>>;

    async fn get_logs(
        &self,
        request: Request<GetLogsRequest>,
    ) -> Result<Response<Self::GetLogsStream>, Status> {
        let config = self.config.clone();
        let input = request.into_inner();
        let directory = orchestrator::stack_dir(&config, &input.name)
            .map_err(|error| status_from_error(&error))?;
        let (sender, receiver) = tokio::sync::mpsc::channel(256);
        tokio::spawn(async move {
            let line_sender = sender.clone();
            let result = tokio::task::spawn_blocking(move || {
                docker::compose_logs(
                    &directory,
                    input.tail.max(1),
                    input.follow,
                    |content| line_sender.blocking_send(Ok(LogLine { content })).is_ok(),
                    || !line_sender.is_closed(),
                )
            })
            .await;
            let error = match result {
                Ok(Err(error)) => Some(status_from_error(&error)),
                Err(error) => Some(Status::internal(format!("后台任务执行失败: {error}"))),
                Ok(Ok(())) => None,
            };
            if let Some(error) = error {
                let _result = sender.send(Err(error)).await;
            }
        });
        Ok(Response::new(ReceiverStream::new(receiver)))
    }
}
