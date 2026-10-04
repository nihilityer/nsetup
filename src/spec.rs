//! 强类型 Compose 中间表示与语义视图。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 项目级 Compose 状态与环境文件转换。
mod project;
/// 服务级镜像与 Traefik 路由语义。
mod service;
/// `spec` 模块单元测试。
#[cfg(test)]
mod tests;
/// 端口、挂载、健康检查与公共校验。
mod value;

pub use value::{
    split_tagged_image, validate_entrypoints, validate_group, validate_hooks, validate_middleware,
    validate_name, validate_user, validate_version,
};

/// 应用模板在项目 `.env` 中持久化服务启动钩子的键。
pub const HOOKS_KEY: &str = "NSETUP_APP_HOOKS_JSON";
/// Traefik router 未声明 entrypoint 时 Traefik 使用的默认入口名。
pub const DEFAULT_ENTRYPOINT: &str = "default";
/// nsetup 生成的路由固定使用的 HTTPS 入口名。
pub const HTTPS_ENTRYPOINT: &str = "https";
/// traefik 模板为自身指标暴露的内网入口名。
pub const METRICS_ENTRYPOINT: &str = "metrics";
/// traefik 模板为自身指标使用的容器端口。
pub const DEFAULT_METRICS_PORT: u16 = 8081;

/// 插入固定的共享反向代理网络定义。
pub fn add_proxy_network(document: &mut Document, external: bool) {
    document.networks.insert(
        String::from("proxy"),
        Network {
            external,
            name: Some(String::from(crate::constants::PROXY_NETWORK)),
        },
    );
}

/// 插入与项目隐式默认网络同名的项目网络定义。
///
/// 服务一旦显式声明 `networks`，Compose 就不再为项目创建 `<项目>_default`；这里用
/// 同名定义把它钉住，使带路由的服务与项目内其他服务仍处于同一个网络上。
pub fn add_project_network(document: &mut Document, stack_name: &str) {
    document.networks.insert(
        String::from("project"),
        Network {
            external: false,
            name: Some(format!("{stack_name}_default")),
        },
    );
}

/// 一个 Traefik router 的路由冲突判定身份。
///
/// Traefik 允许同一 `host` 上存在多条路由，只要它们的路径前缀、入口或后端协议不同；
/// 因此 host 冲突必须按这四个字段的组合判定，而不能只看 `Host()` 规则。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteIdentity {
    /// 路由声明的 DNS 主机名。
    pub host: String,
    /// 归一化的 URL 路径前缀；省略或 `/` 视为匹配整个主机。
    pub path_prefix: String,
    /// 路由监听的 Traefik entrypoint。
    pub entrypoint: String,
    /// Traefik 连接后端时使用的协议。
    pub protocol: RouteProtocol,
}

impl RouteIdentity {
    /// 用路由字段构造归一化身份，并把空 entrypoint 归一为默认入口。
    #[must_use]
    pub fn new(
        host: impl Into<String>,
        path_prefix: Option<&str>,
        entrypoint: &str,
        protocol: RouteProtocol,
    ) -> Self {
        Self {
            host: host.into(),
            path_prefix: normalize_path_prefix(path_prefix),
            entrypoint: normalize_entrypoint(entrypoint),
            protocol,
        }
    }

    /// 返回用于诊断输出的稳定描述，例如 `a.example.com/api (https, http)`。
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} ({}, {})",
            if self.path_prefix.is_empty() {
                self.host.clone()
            } else {
                format!("{}{}", self.host, self.path_prefix)
            },
            self.entrypoint,
            self.protocol.as_str()
        )
    }
}

/// 一条已声明路由及其归属，用于跨项目冲突检查与诊断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteBinding {
    /// 冲突判定身份。
    pub identity: RouteIdentity,
    /// 声明该路由的项目名。
    pub project: String,
    /// 声明该路由的服务名。
    pub service: String,
    /// 稳定的 router 名；用户手写路由使用其 labels 中的原始名称。
    pub router: String,
    /// 是否由 nsetup 从路由声明生成。
    pub managed: bool,
}

impl RouteBinding {
    /// 返回用于报错定位的占用方描述。
    #[must_use]
    pub fn owner(&self) -> String {
        format!(
            "项目 {} 的服务 {} 路由 {}",
            self.project, self.service, self.router
        )
    }
}

/// 将路由路径前缀归一化，使 `/` 与省略等价。
fn normalize_path_prefix(value: Option<&str>) -> String {
    match value.map(str::trim) {
        None | Some("") | Some("/") => String::new(),
        Some(other) => other.trim_end_matches('/').to_string(),
    }
}

/// 将 entrypoint 列表归一化并排序，使声明顺序不影响冲突判定。
fn normalize_entrypoint(value: &str) -> String {
    let mut names: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    if names.is_empty() {
        return String::from(DEFAULT_ENTRYPOINT);
    }
    names.sort_unstable();
    names.dedup();
    names.join(",")
}

/// 完整的受管项目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackSpec {
    /// Compose 项目名。
    pub name: String,
    /// 类型化 Compose 文档。
    pub document: Document,
    /// 持久化到 `.env` 的变量。
    pub environment: BTreeMap<String, String>,
}

/// 受支持的顶层 Compose 文档。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Document {
    /// 以 Compose 服务名为键的服务映射。
    pub services: BTreeMap<String, Service>,
    /// 以逻辑名称为键的项目网络映射。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub networks: BTreeMap<String, Network>,
}

/// 受支持的 Compose 服务字段。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Service {
    /// 包含完整标签的镜像引用。
    pub image: String,
    /// 可选的固定容器名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    /// 替换镜像默认命令的参数。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Compose 重启策略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<String>,
    /// 宿主机或其他直接 Docker 网络模式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_mode: Option<String>,
    /// 服务加入的具名 Compose 网络。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<String>,
    /// 短语法发布端口映射。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    /// 短语法 bind mount。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    /// 映射形式的容器环境变量。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// 容器读取的环境变量文件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_file: Vec<String>,
    /// 列表形式的 Docker label。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// 覆盖镜像内置用户的 `UID[:GID]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// 除主用户组外额外加入的补充组。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_add: Vec<String>,
    /// 可选容器健康检查。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<Healthcheck>,
    /// 可选 Docker 日志配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<Logging>,
}

/// 受支持的 Compose 网络定义。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// 是否要求 Docker 中已存在该网络。
    #[serde(default, skip_serializing_if = "is_false")]
    pub external: bool,
    /// 可选的固定 Docker 网络名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 受支持的 Compose 健康检查定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Healthcheck {
    /// Compose 健康检查测试，为 `[CMD, ..]` 或 `[CMD-SHELL, command]` 形式。
    pub test: Vec<String>,
    /// 检查间隔。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// 超时时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// 启动宽限期。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_period: Option<String>,
    /// 重试次数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
}

/// 受支持的 Compose 日志定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Logging {
    /// Docker 日志驱动。
    pub driver: String,
    /// 驱动专用选项。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
}

/// 一个服务的启动钩子。
///
/// 钩子在 daemon 上以项目目录为工作目录执行，因此同一份声明不依赖 TOML 文件的
/// 绝对位置。`pre_start` 在 Compose 启动之前执行，`post_start` 在之后执行。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceHooks {
    /// Compose 启动前按顺序执行的 shell 命令。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_start: Vec<String>,
    /// Compose 启动后按顺序执行的 shell 命令。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_start: Vec<String>,
}

impl ServiceHooks {
    /// 判断钩子是否为空。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pre_start.is_empty() && self.post_start.is_empty()
    }

    /// 返回指定阶段的命令列表。
    #[must_use]
    pub fn commands(&self, stage: HookStage) -> &[String] {
        match stage {
            HookStage::PreStart => &self.pre_start,
            HookStage::PostStart => &self.post_start,
        }
    }
}

/// 启动钩子的执行阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStage {
    /// Compose 启动之前。
    PreStart,
    /// Compose 启动之后。
    PostStart,
}

impl HookStage {
    /// 返回稳定名称与中文说明。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PreStart => "pre_start",
            Self::PostStart => "post_start",
        }
    }
}

/// 从 label 派生的 Traefik 语义路由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// 在 TOML、Compose router 和 backend 中保持稳定的路由名。
    pub name: String,
    /// 此路由器处理的 DNS 主机名。
    pub hosts: Vec<String>,
    /// 可选 URL 路径前缀。
    pub path_prefix: Option<String>,
    /// 目标容器端口。
    pub container_port: u16,
    /// 不含提供者后缀的中间件名称。
    pub middlewares: Vec<String>,
    /// 后端协议。
    pub protocol: RouteProtocol,
    /// 路由监听的 entrypoint。
    pub entrypoint: String,
    /// 是否启用负载均衡粘性 Cookie。
    pub sticky_cookie: bool,
    /// 可选的 `passHostHeader` 覆盖值。
    pub pass_host_header: Option<bool>,
    /// 可选路由优先级。
    pub priority: Option<u32>,
}

/// 用户手写 label 声明的一条 Traefik 路由。
///
/// 与 nsetup 从 `[services.*.traefik]` 生成的 [`Route`] 不同，这里保留 label 里的
/// 原始表达，供 `nsetup show --routes` 与冲突检查使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRoute {
    /// label 中的原始 router 名。
    pub router: String,
    /// 规则中声明的全部主机名。
    pub hosts: Vec<String>,
    /// 规则中声明的可选路径前缀。
    pub path_prefix: Option<String>,
    /// 监听的 entrypoint 列表，已归一化排序。
    pub entrypoint: String,
    /// 后端协议。
    pub protocol: RouteProtocol,
    /// 后端容器端口；label 未声明时为 `None`。
    pub container_port: Option<u16>,
    /// 引用的中间件，保留原始 provider 后缀。
    pub middlewares: Vec<String>,
    /// 显式优先级。
    pub priority: Option<u32>,
}

/// 受支持的 Traefik 后端协议。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum RouteProtocol {
    /// 普通 HTTP。
    #[default]
    Http,
    /// 连接后端的 HTTPS。
    Https,
    /// 明文 HTTP/2。
    H2c,
}

impl RouteProtocol {
    /// 返回 Traefik scheme 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::H2c => "h2c",
        }
    }

    /// 解析受支持的协议。
    ///
    /// # 错误
    ///
    /// 值不受支持时返回错误。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            "h2c" => Ok(Self::H2c),
            _ => anyhow::bail!("不支持的路由协议: {value}"),
        }
    }
}

/// 用于校验和编辑的已解析发布端口。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublishedPort {
    /// 可选的宿主机 IP 绑定。
    pub host_ip: Option<String>,
    /// 宿主机端口。
    pub host_port: u16,
    /// 容器端口。
    pub container_port: u16,
    /// 传输协议。
    pub protocol: PortProtocol,
}

/// 受支持的发布端口协议。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum PortProtocol {
    /// 传输控制协议（TCP）。
    #[default]
    Tcp,
    /// 用户数据报协议（UDP）。
    Udp,
}

impl PortProtocol {
    /// 返回 Compose 后缀。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// 已解析的 bind mount。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindMount {
    /// 宿主机源绝对路径。
    pub host_path: String,
    /// 容器目标绝对路径。
    pub container_path: String,
    /// 只读标志。
    pub read_only: bool,
}

/// 用于省略 `false` 值的 Serde 辅助函数。
fn is_false(value: &bool) -> bool {
    !value
}
