//! Nihility 基础设施与应用配置生成。

mod app;
mod compose;
mod infra;
mod types;

pub use app::{generate_application, generate_static_site};
pub use infra::generate_infrastructure;
pub use types::{
    AppSpec, GeneratedFile, GeneratedStack, InfraSpec, Middleware, NamedVolume, NetworkMode,
    PortProtocol, PublishedPort, Route, StaticAsset, StaticSiteSpec, Volume,
};
