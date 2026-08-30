//! gRPC 协议、服务端实现、传输层与客户端。

use crate::config::{Config, set_mode};
use crate::constants::{ADMIN_GROUP, GRPC_SOCKET, MAX_RPC_MESSAGE_SIZE};
use crate::install::ensure_auth_token;
use crate::orchestrator::{Asset, Edit, NetworkEdit, Orchestrator, StackInfo};
use crate::spec::{BindMount, Healthcheck, PortProtocol, PublishedPort, Route, RouteProtocol};
use anyhow::Context;
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::Stream;
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint, Server};
use tonic::{Request, Response, Status};
use tower::service_fn;

/// 从带完整注释的协议定义生成的代码。
#[allow(
    clippy::doc_markdown,
    clippy::missing_const_for_fn,
    clippy::missing_docs_in_private_items
)]
pub mod proto {
    tonic::include_proto!("nsetup.v1");
}

use proto::orchestrator_client::OrchestratorClient;
use proto::orchestrator_server::{Orchestrator as OrchestratorRpc, OrchestratorServer};

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

/// 同时支持 Unix socket 与认证 TCP 通道的客户端。
#[derive(Debug, Clone)]
pub struct RpcClient {
    /// 生成的 Tonic 客户端。
    inner: OrchestratorClient<Channel>,
    /// 可选的 TCP 认证元数据。
    authorization: Option<MetadataValue<Ascii>>,
}

impl RpcClient {
    /// 连接所选守护进程端点。
    ///
    /// # 错误
    ///
    /// 传输失败或缺少 TCP 凭据时返回错误。
    pub async fn connect(
        endpoint: Option<&str>,
        token_file: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let endpoint = endpoint
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("unix://{GRPC_SOCKET}"));
        if let Some(path) = endpoint.strip_prefix("unix://") {
            if token_file.is_some() {
                anyhow::bail!("Unix socket 连接不使用 --token-file");
            }
            return Self::connect_unix(path).await;
        }
        Self::connect_tcp(endpoint, token_file).await
    }

    /// 调用状态 RPC。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn status(&mut self) -> anyhow::Result<proto::StatusResponse> {
        let request = self.request(proto::StatusRequest {});
        Ok(self.inner.status(request).await?.into_inner())
    }

    /// 应用 TOML 声明。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn apply(
        &mut self,
        request: proto::ApplyRequest,
    ) -> anyhow::Result<proto::OperationResponse> {
        let request = self.request(request);
        Ok(self.inner.apply(request).await?.into_inner())
    }

    /// 导入 Compose 状态。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn import_compose(
        &mut self,
        request: proto::ImportComposeRequest,
    ) -> anyhow::Result<proto::OperationResponse> {
        let request = self.request(request);
        Ok(self.inner.import_compose(request).await?.into_inner())
    }

    /// 导出当前项目状态。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn export(&mut self, name: String) -> anyhow::Result<proto::ExportResponse> {
        let request = self.request(proto::ExportRequest { name });
        Ok(self.inner.export(request).await?.into_inner())
    }

    /// 应用部分服务修改。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn edit(
        &mut self,
        edit: proto::EditRequest,
    ) -> anyhow::Result<proto::OperationResponse> {
        let request = self.request(edit);
        Ok(self.inner.edit(request).await?.into_inner())
    }

    /// 列出受管项目。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn list(&mut self) -> anyhow::Result<proto::ListResponse> {
        let request = self.request(proto::ListRequest {});
        Ok(self.inner.list(request).await?.into_inner())
    }

    /// 获取一个受管项目。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn get(&mut self, name: String) -> anyhow::Result<proto::Stack> {
        let request = self.request(proto::GetRequest { name });
        Ok(self.inner.get(request).await?.into_inner())
    }

    /// 删除已经确认的项目。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn remove(&mut self, name: String) -> anyhow::Result<proto::OperationResponse> {
        let request = self.request(proto::RemoveRequest { name, force: true });
        Ok(self.inner.remove(request).await?.into_inner())
    }

    /// 调用非流式生命周期 RPC。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn action(
        &mut self,
        name: String,
        action: Action,
    ) -> anyhow::Result<proto::OperationResponse> {
        let request = self.request(proto::ActionRequest { name });
        let response = match action {
            Action::Start => self.inner.start(request).await?,
            Action::Stop => self.inner.stop(request).await?,
            Action::Restart => self.inner.restart(request).await?,
            Action::Build => self.inner.build(request).await?,
        };
        Ok(response.into_inner())
    }

    /// 发起服务端流式镜像拉取。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn pull(
        &mut self,
        name: String,
    ) -> anyhow::Result<tonic::Streaming<proto::PullProgress>> {
        let request = self.request(proto::ActionRequest { name });
        Ok(self.inner.pull(request).await?.into_inner())
    }

    /// 发起服务端流式日志调用。
    ///
    /// # 错误
    ///
    /// RPC 或传输失败时返回错误。
    pub async fn logs(
        &mut self,
        name: String,
        tail: u32,
        follow: bool,
    ) -> anyhow::Result<tonic::Streaming<proto::LogLine>> {
        let request = self.request(proto::LogsRequest { name, tail, follow });
        Ok(self.inner.logs(request).await?.into_inner())
    }

    /// 通过 Unix 域套接字连接 Tonic 通道。
    ///
    /// # 错误
    ///
    /// 无法连接套接字时返回错误。
    async fn connect_unix(path: &str) -> anyhow::Result<Self> {
        let socket = PathBuf::from(path);
        let connector = socket.clone();
        let channel = Endpoint::try_from("http://[::]:50051")?
            .connect_with_connector(service_fn(move |_| {
                let path = connector.clone();
                async move { UnixStream::connect(path).await.map(TokioIo::new) }
            }))
            .await
            .with_context(|| format!("无法连接本机 gRPC socket: {}", socket.display()))?;
        Ok(Self {
            inner: configured_client(channel),
            authorization: None,
        })
    }

    /// 连接经过认证的 TCP Tonic 通道。
    ///
    /// # 错误
    ///
    /// 令牌或端点无效时返回错误。
    async fn connect_tcp(endpoint: String, token_file: Option<&Path>) -> anyhow::Result<Self> {
        let token_path =
            token_file.ok_or_else(|| anyhow::anyhow!("TCP 连接必须提供 --token-file"))?;
        let token = std::fs::read_to_string(token_path)
            .with_context(|| format!("无法读取认证 token: {}", token_path.display()))?;
        let authorization = format!("Bearer {}", token.trim())
            .parse()
            .map_err(|error| anyhow::anyhow!("认证 token 不能作为 gRPC 元数据: {error}"))?;
        let endpoint = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint
        } else {
            format!("http://{}", endpoint.trim_start_matches("tcp://"))
        };
        let channel = Channel::from_shared(endpoint.clone())?
            .connect()
            .await
            .with_context(|| format!("无法连接远程 daemon: {endpoint}"))?;
        Ok(Self {
            inner: configured_client(channel),
            authorization: Some(authorization),
        })
    }

    /// 包装消息并附加 TCP 认证元数据。
    fn request<T>(&self, message: T) -> Request<T> {
        let mut request = Request::new(message);
        if let Some(authorization) = &self.authorization {
            request
                .metadata_mut()
                .insert("authorization", authorization.clone());
        }
        request
    }
}

/// 客户端包装层公开的非流式生命周期操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 执行 Compose 启动。
    Start,
    /// 执行 Compose 停止。
    Stop,
    /// 执行 Compose 重启。
    Restart,
    /// 执行 Compose 构建。
    Build,
}

/// 启动配置的 gRPC 监听器并运行到收到终止信号。
///
/// # 错误
///
/// 监听器、权限、认证或服务器发生故障时返回错误。
pub async fn serve(config: Config) -> anyhow::Result<()> {
    if let Some(path) = config.listen.strip_prefix("unix://") {
        return serve_unix(config.clone(), PathBuf::from(path)).await;
    }
    serve_tcp(config).await
}

/// 通过受访问控制的 Unix 域套接字提供服务。
///
/// # 错误
///
/// 套接字准备或服务过程失败时返回错误。
async fn serve_unix(config: Config, socket: PathBuf) -> anyhow::Result<()> {
    prepare_socket(&socket)?;
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("无法绑定 gRPC socket: {}", socket.display()))?;
    set_mode(&socket, 0o660)?;
    let output = Command::new("chown")
        .arg(format!("root:{ADMIN_GROUP}"))
        .arg(&socket)
        .output()
        .context("无法设置 gRPC socket 属组")?;
    if !output.status.success() {
        anyhow::bail!(
            "无法设置 gRPC socket 属组: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    tracing::info!("守护进程正在监听 unix://{}", socket.display());
    let result = Server::builder()
        .add_service(configured_server(RpcService::new(config)?))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown_signal())
        .await;
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    result?;
    Ok(())
}

/// 通过经过认证的 TCP 地址提供服务。
///
/// # 错误
///
/// 令牌加载、地址解析或服务过程失败时返回错误。
async fn serve_tcp(config: Config) -> anyhow::Result<()> {
    let address = config
        .listen
        .strip_prefix("tcp://")
        .unwrap_or(&config.listen)
        .parse()
        .context("gRPC TCP 监听地址无效")?;
    let token = ensure_auth_token()?;
    let server = configured_server(RpcService::new(config)?);
    let authenticated =
        InterceptedService::new(server, move |request| authenticate(request, &token));
    tracing::info!("守护进程正在监听 {address}");
    Server::builder()
        .add_service(authenticated)
        .serve_with_shutdown(address, shutdown_signal())
        .await?;
    Ok(())
}

/// 配置生成服务端的消息大小限制。
fn configured_server(service: RpcService) -> OrchestratorServer<RpcService> {
    OrchestratorServer::new(service)
        .max_decoding_message_size(MAX_RPC_MESSAGE_SIZE)
        .max_encoding_message_size(MAX_RPC_MESSAGE_SIZE)
}

/// 配置生成客户端的消息大小限制。
fn configured_client(channel: Channel) -> OrchestratorClient<Channel> {
    OrchestratorClient::new(channel)
        .max_decoding_message_size(MAX_RPC_MESSAGE_SIZE)
        .max_encoding_message_size(MAX_RPC_MESSAGE_SIZE)
}

/// 仅删除失效套接字，并创建其父目录。
///
/// # 错误
///
/// 遇到非套接字路径时返回错误，而不覆盖该路径。
fn prepare_socket(socket: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let parent = socket
        .parent()
        .ok_or_else(|| anyhow::anyhow!("socket 缺少父目录: {}", socket.display()))?;
    std::fs::create_dir_all(parent)?;
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(socket)?,
        Ok(_) => anyhow::bail!("拒绝覆盖非 socket 路径: {}", socket.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// 使用固定工作量比较校验 TCP Bearer 元数据。
fn authenticate(mut request: Request<()>, expected: &str) -> Result<Request<()>, Status> {
    let actual = request
        .metadata()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if actual.is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes())) {
        request.extensions_mut().insert(Authenticated);
        Ok(request)
    } else {
        Err(Status::unauthenticated("认证 token 无效或缺失"))
    }
}

/// 记录 TCP 认证成功的标记。
#[derive(Debug, Clone, Copy)]
struct Authenticated;

/// 比较密钥，且不根据内容提前返回。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let left_byte = left.get(index).copied().unwrap_or_default();
        let right_byte = right.get(index).copied().unwrap_or_default();
        difference |= usize::from(left_byte ^ right_byte);
    }
    difference == 0
}

/// 等待 Ctrl-C 或 Unix 终止信号。
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            if let Err(error) = ctrl_c.await {
                tracing::error!("无法监听 Ctrl-C: {error}");
            }
            return;
        };
        tokio::select! {
            result = ctrl_c => {
                if let Err(error) = result {
                    tracing::error!("无法监听 Ctrl-C: {error}");
                }
            }
            _signal = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = ctrl_c.await {
        tracing::error!("无法监听 Ctrl-C: {error}");
    }
}

/// 将用户输入错误或操作错误转换为 RPC 状态。
fn status_from_error(error: &anyhow::Error) -> Status {
    Status::invalid_argument(format!("{error:#}"))
}

/// 将成功消息包装为 Tonic 响应。
fn operation_response(message: String) -> Response<proto::OperationResponse> {
    Response::new(proto::OperationResponse { message })
}

/// 将管理器项目信息转换为线路表示。
fn stack_to_proto(stack: StackInfo) -> proto::Stack {
    proto::Stack {
        name: stack.name,
        services: stack.services,
        compose_yaml: stack.compose_yaml,
        environment: stack.environment.into_iter().collect(),
        status: stack.status,
    }
}

/// 转换并校验线路修改表示。
///
/// # 错误
///
/// 枚举值无效或数值越界时返回错误。
fn edit_from_proto(request: proto::EditRequest) -> anyhow::Result<Edit> {
    let routes = if request.routes.is_empty() {
        None
    } else {
        Some(
            request
                .routes
                .into_iter()
                .map(route_from_proto)
                .collect::<anyhow::Result<_>>()?,
        )
    };
    let published_ports = if request.published_ports.is_empty() {
        None
    } else {
        Some(
            request
                .published_ports
                .into_iter()
                .map(port_from_proto)
                .collect::<anyhow::Result<_>>()?,
        )
    };
    let volumes = request
        .volumes
        .into_iter()
        .map(|volume| {
            let mount = BindMount {
                host_path: volume.host_path,
                container_path: volume.container_path,
                read_only: volume.read_only,
            };
            BindMount::parse(&mount.compose_value())
        })
        .collect::<anyhow::Result<_>>()?;
    let middlewares = if request.middlewares.is_empty() {
        None
    } else {
        Some(middleware_names(&request.middlewares)?)
    };
    let network = request
        .network_mode
        .map(|value| network_from_proto(value, request.external_network.as_deref()))
        .transpose()?;
    let healthcheck = request
        .healthcheck
        .map(healthcheck_from_proto)
        .transpose()?;
    Ok(Edit {
        service: request.service,
        image: request.image,
        version: request.version,
        command: (!request.command.is_empty()).then_some(request.command),
        container_port: request
            .container_port
            .map(|value| u16::try_from(value).context("container_port 超出范围"))
            .transpose()?,
        routes,
        published_ports,
        volumes,
        environment: request.environment.into_iter().collect(),
        network,
        middlewares,
        labels: request.labels,
        healthcheck,
        remove_healthcheck: request.remove_healthcheck,
        start: request.start,
    })
}

/// 将一条线路路由转换为语义视图。
///
/// # 错误
///
/// 协议、中间件值或端口无效时返回错误。
fn route_from_proto(route: proto::Route) -> anyhow::Result<Route> {
    Ok(Route {
        hosts: route.hosts,
        path_prefix: route.path_prefix,
        container_port: u16::try_from(route.container_port).context("路由端口超出范围")?,
        middlewares: middleware_names(&route.middlewares)?,
        protocol: match proto::RouteProtocol::try_from(route.protocol)? {
            proto::RouteProtocol::Unspecified | proto::RouteProtocol::Http => RouteProtocol::Http,
            proto::RouteProtocol::Https => RouteProtocol::Https,
            proto::RouteProtocol::H2c => RouteProtocol::H2c,
        },
        sticky_cookie: route.sticky_cookie,
        pass_host_header: route.pass_host_header,
        priority: route.priority,
    })
}

/// 转换一条线路发布端口映射。
///
/// # 错误
///
/// 协议无效或数值越界时返回错误。
fn port_from_proto(port: proto::PublishedPort) -> anyhow::Result<PublishedPort> {
    let value = PublishedPort {
        host_ip: port.host_ip,
        host_port: u16::try_from(port.host_port).context("宿主机端口超出范围")?,
        container_port: u16::try_from(port.container_port).context("容器端口超出范围")?,
        protocol: match proto::PortProtocol::try_from(port.protocol)? {
            proto::PortProtocol::Unspecified | proto::PortProtocol::Tcp => PortProtocol::Tcp,
            proto::PortProtocol::Udp => PortProtocol::Udp,
        },
    };
    PublishedPort::parse(&value.compose_value())
}

/// 转换线路网络变更。
///
/// # 错误
///
/// 值未指定或缺少外部网络名时返回错误。
fn network_from_proto(value: i32, external: Option<&str>) -> anyhow::Result<NetworkEdit> {
    match proto::NetworkMode::try_from(value)? {
        proto::NetworkMode::Unspecified => anyhow::bail!("network_mode 不能为 unspecified"),
        proto::NetworkMode::Bridge => Ok(NetworkEdit::Bridge),
        proto::NetworkMode::Host => Ok(NetworkEdit::Host),
        proto::NetworkMode::External => Ok(NetworkEdit::External(
            external
                .filter(|name| !name.is_empty())
                .ok_or_else(|| anyhow::anyhow!("external 网络缺少 external_network"))?
                .to_string(),
        )),
    }
}

/// 将中间件枚举数值转换为稳定的模板名称。
///
/// # 错误
///
/// 枚举值未知或未指定时返回错误。
fn middleware_names(values: &[i32]) -> anyhow::Result<Vec<String>> {
    values
        .iter()
        .map(|value| match proto::Middleware::try_from(*value)? {
            proto::Middleware::Unspecified => anyhow::bail!("middleware 不能为 unspecified"),
            proto::Middleware::Gzip => Ok(String::from("gzip")),
            proto::Middleware::ForwardedHeaders => Ok(String::from("forwarded-headers")),
            proto::Middleware::InternalOnly => Ok(String::from("internal-only")),
            proto::Middleware::Tls => Ok(String::from("tls")),
        })
        .collect()
}

/// 将线路健康检查转换为 Compose IR 表示。
///
/// # 错误
///
/// 命令为空或重试次数无效时返回错误。
fn healthcheck_from_proto(value: proto::Healthcheck) -> anyhow::Result<Healthcheck> {
    if value.command.is_empty() {
        anyhow::bail!("healthcheck command 不能为空");
    }
    if value.retries == Some(0) {
        anyhow::bail!("healthcheck retries 必须大于 0");
    }
    let mut output = Healthcheck::command(value.command);
    output.interval = value.interval;
    output.timeout = value.timeout;
    output.start_period = value.start_period;
    output.retries = value.retries;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn secret_comparison_checks_length_and_content() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"taken"));
        assert!(!constant_time_eq(b"token", b"token-long"));
    }
}
