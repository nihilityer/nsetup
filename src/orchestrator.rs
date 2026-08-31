//! 项目编排、校验、编辑与单一部署收口。

use crate::config::Config;
use crate::spec::{BindMount, Healthcheck, PublishedPort, Route};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 项目部署、冲突检查与目录解析。
mod deploy;
/// 服务局部编辑与网络修改。
mod edit;
/// 项目生命周期与查询操作。
mod operations;
/// 安全路径与项目文件操作。
mod storage;

/// daemon 侧有状态项目管理器。
#[derive(Debug, Clone)]
pub struct Orchestrator {
    /// 已校验的 daemon 配置。
    config: Config,
}

/// 随应用请求上传的附属文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// 站点相对路径。
    pub path: PathBuf,
    /// 文件原始字节。
    pub content: Vec<u8>,
}

/// 服务局部修改。
#[derive(Debug, Clone, Default)]
pub struct Edit {
    /// 可选服务选择器。
    pub service: Option<String>,
    /// 可选的新镜像仓库。
    pub image: Option<String>,
    /// 可选的新镜像版本。
    pub version: Option<String>,
    /// 可选的完整命令替换。
    pub command: Option<Vec<String>>,
    /// 可选的默认目标端口，应用于现有路由。
    pub container_port: Option<u16>,
    /// 可选的完整路由替换。
    pub routes: Option<Vec<Route>>,
    /// 可选的完整发布端口替换。
    pub published_ports: Option<Vec<PublishedPort>>,
    /// 追加到服务的 bind mount。
    pub volumes: Vec<BindMount>,
    /// 按键合并的环境变量。
    pub environment: BTreeMap<String, String>,
    /// 可选网络修改。
    pub network: Option<NetworkEdit>,
    /// 可选的中间件替换，应用于全部保留路由。
    pub middlewares: Option<Vec<String>>,
    /// 按键替换的自定义 label。
    pub labels: Vec<String>,
    /// 可选的新健康检查。
    pub healthcheck: Option<Healthcheck>,
    /// 是否移除当前健康检查。
    pub remove_healthcheck: bool,
    /// 是否启动编辑后的服务。
    pub start: bool,
}

/// `edit` 使用的网络修改。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkEdit {
    /// Compose bridge 网络；存在路由时同时加入代理网络。
    Bridge,
    /// 宿主机网络。
    Host,
    /// 指定名称的外部网络。
    External(String),
}

/// `list` 和 `get` 返回的项目信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackInfo {
    /// 项目名。
    pub name: String,
    /// 服务名列表。
    pub services: Vec<String>,
    /// 规范化 Compose YAML。
    pub compose_yaml: String,
    /// 项目 `.env` 键值。
    pub environment: BTreeMap<String, String>,
    /// 尽力获取的 Docker Compose 状态 JSON。
    pub status: String,
}
