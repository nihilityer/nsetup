//! Compose 编排领域类型。

use std::path::PathBuf;

/// 调用方提供的项目名或 Compose 内容无效。
#[derive(Debug, Clone)]
pub struct InvalidInput(pub String);

impl std::fmt::Display for InvalidInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidInput {}

/// 请求的 Compose 项目不存在。
#[derive(Debug, Clone)]
pub struct StackNotFound(pub String);

impl std::fmt::Display for StackNotFound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for StackNotFound {}

/// 支持的 Compose 项目生命周期操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackAction {
    /// 启动项目。
    Start,
    /// 停止项目。
    Stop,
    /// 重启项目。
    Restart,
    /// 构建项目镜像。
    Build,
}

/// Docker 容器运行状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ContainerState {
    /// Docker 未提供状态，或返回了未知状态。
    #[default]
    Unknown,
    /// 容器已创建但尚未运行。
    Created,
    /// 容器正在运行。
    Running,
    /// 容器已暂停。
    Paused,
    /// 容器正在重启。
    Restarting,
    /// 容器正在删除。
    Removing,
    /// 容器已经退出。
    Exited,
    /// 容器处于不可恢复状态。
    Dead,
}

impl ContainerState {
    /// 将 Docker Compose JSON 中的状态转换为内部枚举。
    #[must_use]
    pub(super) fn from_docker(value: &str) -> Self {
        match value {
            "created" => Self::Created,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "restarting" => Self::Restarting,
            "removing" => Self::Removing,
            "exited" | "stopped" => Self::Exited,
            "dead" => Self::Dead,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for ContainerState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Unknown => "未知",
            Self::Created => "已创建",
            Self::Running => "运行中",
            Self::Paused => "已暂停",
            Self::Restarting => "重启中",
            Self::Removing => "删除中",
            Self::Exited => "已退出",
            Self::Dead => "不可恢复",
        };
        formatter.write_str(label)
    }
}

/// Docker 容器健康检查状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ContainerHealth {
    /// 容器未配置健康检查。
    #[default]
    None,
    /// 健康检查正在等待初始结果。
    Starting,
    /// 健康检查通过。
    Healthy,
    /// 健康检查失败。
    Unhealthy,
    /// Docker 返回了未知健康状态。
    Unknown,
}

impl ContainerHealth {
    /// 将 Docker Compose JSON 中的健康状态转换为内部枚举。
    #[must_use]
    pub(super) fn from_docker(value: &str) -> Self {
        match value {
            "" | "none" => Self::None,
            "starting" => Self::Starting,
            "healthy" => Self::Healthy,
            "unhealthy" => Self::Unhealthy,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for ContainerHealth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::None => "未配置",
            Self::Starting => "检查中",
            Self::Healthy => "健康",
            Self::Unhealthy => "不健康",
            Self::Unknown => "未知",
        };
        formatter.write_str(label)
    }
}

/// Compose 项目中的容器状态。
#[derive(Debug, Clone, Default)]
pub struct ContainerInfo {
    /// Compose 服务名。
    pub service: String,
    /// 容器名。
    pub name: String,
    /// 容器运行状态。
    pub state: ContainerState,
    /// 容器健康状态。
    pub health: ContainerHealth,
}

/// Compose 项目的基本信息。
#[derive(Debug, Clone)]
pub struct StackInfo {
    /// 项目名。
    pub name: String,
    /// Compose 项目目录。
    pub project_directory: PathBuf,
    /// Compose 文件路径。
    pub compose_file: PathBuf,
    /// 环境变量文件路径。
    pub env_file: PathBuf,
    /// 项目中的容器状态。
    pub containers: Vec<ContainerInfo>,
}
