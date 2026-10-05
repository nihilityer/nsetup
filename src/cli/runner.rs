//! CLI 本地命令与远程 RPC 命令执行。

use super::args::{AssetsModeArg, AssetsPermsArg, Cli, Command};
use super::edit::edit_request;
use super::io::{
    compact_status, read_assets, read_files, read_limited, stdin_is_terminal, write_diagnostic,
    write_line, write_new_file, write_text,
};
use super::progress::PullProgressRenderer;
use crate::constants::MAX_CONFIG_SIZE;
use crate::install::{InstallOptions, install};
use crate::rpc::proto;
use crate::rpc::{Action, RpcClient};
use crate::template::{TemplateKind, skeleton};
use anyhow::Context;
use tokio_stream::StreamExt;

/// 执行已解析的命令。
///
/// # 错误
///
/// 输入无效、文件操作失败、RPC 失败或 daemon 操作失败时返回错误。
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init(args) => {
            install(InstallOptions {
                domain: args.domain,
                stacks_root: args.stacks_root,
                data_roots: (!args.data_roots.is_empty()).then_some(args.data_roots),
                listen: args.listen,
                docker_socket: args.docker_socket,
                force: args.force,
            })?;
            write_line("nsetup daemon 已安装并启动")?;
            return Ok(());
        }
        Command::Template(args) => {
            let kind = TemplateKind::parse(&args.name)?;
            write_text(skeleton(kind))?;
            return Ok(());
        }
        Command::Daemon => {
            let config = crate::config::Config::load()?;
            return crate::rpc::serve(config).await;
        }
        _ => {}
    }
    let mut client = RpcClient::connect(cli.endpoint.as_deref(), cli.token_file.as_deref()).await?;
    dispatch_remote(&mut client, cli.command).await
}

/// 建立 daemon 连接后分发单个命令。
///
/// # 错误
///
/// 文件、输入、流式传输或 RPC 操作失败时返回错误。
async fn dispatch_remote(client: &mut RpcClient, command: Command) -> anyhow::Result<()> {
    match command {
        Command::Status => {
            let status = client.status().await?;
            write_line(&format!(
                "版本: {}\nDocker: {}\n项目根目录: {}\n主域名: {}",
                status.version,
                if status.docker_available {
                    "可用"
                } else {
                    "不可用"
                },
                status.stacks_root,
                status.domain
            ))?;
        }
        Command::Up(args) => run_up(client, args).await?,
        Command::Import(args) => {
            let compose_yaml = read_limited(&args.file, MAX_CONFIG_SIZE)?;
            let env_file = args
                .env_file
                .as_deref()
                .map(|path| read_limited(path, MAX_CONFIG_SIZE))
                .transpose()?;
            let stream = client
                .import_compose(proto::ImportComposeRequest {
                    name: args.name,
                    compose_yaml,
                    env_file,
                    start: args.start,
                })
                .await?;
            write_operation_stream(stream).await?;
        }
        Command::Export(args) => run_export(client, args).await?,
        Command::Edit(args) => {
            let stream = client.edit(edit_request(*args)?).await?;
            write_operation_stream(stream).await?;
        }
        Command::List => {
            let response = client.list().await?;
            if response.stacks.is_empty() {
                write_line("没有受管项目")?;
            } else {
                write_line("项目\t服务\t状态")?;
                for stack in response.stacks {
                    write_line(&format!(
                        "{}\t{}\t{}",
                        stack.name,
                        stack.services.join(","),
                        compact_status(&stack.status)
                    ))?;
                }
            }
        }
        Command::Show(args) => {
            let stack = client.get(args.name).await?;
            write_text(&format!(
                "项目名: {}\n服务: {}\n状态: {}\n",
                stack.name,
                stack.services.join(", "),
                stack.status
            ))?;
            if args.routes {
                write_text(&render_routes(&stack.name, &stack.compose_yaml)?)?;
            } else {
                write_text(&format!("\n{}", stack.compose_yaml))?;
            }
        }
        Command::Doctor => run_doctor(client).await?,
        Command::Config(args) => run_config(client, args).await?,
        Command::Start(args) => {
            run_action(client, args.name, Action::Start).await?;
        }
        Command::Stop(args) => {
            run_action(client, args.name, Action::Stop).await?;
        }
        Command::Restart(args) => {
            run_action(client, args.name, Action::Restart).await?;
        }
        Command::Build(args) => {
            run_action(client, args.name, Action::Build).await?;
        }
        Command::Pull(args) => run_pull(client, args.name).await?,
        Command::Logs(args) => {
            let mut stream = client.logs(args.name, args.tail, args.follow).await?;
            while let Some(line) = stream.next().await.transpose()? {
                write_line(&line.line)?;
            }
        }
        Command::Remove(args) => run_remove(client, args.name, args.force).await?,
        Command::Init(_) | Command::Template(_) | Command::Daemon => {
            anyhow::bail!("本地命令被错误地发送到 daemon 分发器");
        }
    }
    Ok(())
}

/// 读取声明与附属文件后发起 `up`。
async fn run_up(client: &mut RpcClient, args: super::args::UpArgs) -> anyhow::Result<()> {
    let config_toml = read_limited(&args.file, MAX_CONFIG_SIZE)?;
    let assets = args
        .assets
        .as_deref()
        .map(read_assets)
        .transpose()?
        .unwrap_or_default();
    let project_files = read_files(&args.files)?;
    if args.files_only && assets.is_empty() && project_files.is_empty() {
        anyhow::bail!("--files-only 需要同时提供 --files 或 --assets，否则没有可同步的内容");
    }
    let stream = client
        .apply(proto::ApplyRequest {
            config_toml,
            assets,
            start: args.start,
            force: args.force,
            project_files,
            restart_dependents: args.restart_dependents,
            assets_provided: args.assets.is_some(),
            assets_mode: match args.assets_mode {
                AssetsModeArg::Merge => proto::AssetsMode::Merge as i32,
                AssetsModeArg::Replace => proto::AssetsMode::Replace as i32,
            },
            files_into: args.files_into,
            assets_perms: match args.assets_perms {
                AssetsPermsArg::WorldReadable => proto::AssetsPerms::WorldReadable as i32,
                AssetsPermsArg::Private => proto::AssetsPerms::Private as i32,
            },
            files_only: args.files_only,
        })
        .await?;
    write_operation_stream(stream).await
}

/// 导出项目声明并写入文件或标准输出。
async fn run_export(client: &mut RpcClient, args: super::args::ExportArgs) -> anyhow::Result<()> {
    let response = client.export(args.name, args.keep_comments).await?;
    match args.output {
        Some(path) => write_new_file(&path, response.config_toml.as_bytes()),
        None => write_text(&response.config_toml),
    }
}

/// 输出 Traefik 接管情况报告，并在发现问题时返回非零退出状态。
async fn run_doctor(client: &mut RpcClient) -> anyhow::Result<()> {
    let response = client.doctor().await?;
    write_text(&format!("{}\n", response.report))?;
    if response.problems > 0 {
        anyhow::bail!("doctor 发现 {} 个问题", response.problems);
    }
    Ok(())
}

/// 执行一个流式生命周期操作。
async fn run_action(client: &mut RpcClient, name: String, action: Action) -> anyhow::Result<()> {
    let stream = client.action(name, action).await?;
    write_operation_stream(stream).await
}

/// 拉取镜像并选择适合当前 stdout 的进度渲染。
async fn run_pull(client: &mut RpcClient, name: String) -> anyhow::Result<()> {
    let mut stream = client.pull(name).await?;
    let mut renderer = PullProgressRenderer::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(progress) => renderer.render(&progress)?,
            Err(error) => {
                renderer.finish(false)?;
                return Err(error.into());
            }
        }
    }
    renderer.finish(true)
}

/// 完成确认后流式删除项目。
///
/// 非交互场景（管道、脚本、CI）不再返回 `not a terminal`，而是明确提示使用 `--force`。
async fn run_remove(client: &mut RpcClient, name: String, force: bool) -> anyhow::Result<()> {
    if !force && !stdin_is_terminal() {
        anyhow::bail!("标准输入不是终端，无法交互确认删除项目 {name}；确认删除请使用 --force");
    }
    let confirmed = force
        || dialoguer::Confirm::new()
            .with_prompt(format!("停止并删除项目 {name}？bind mount 数据会保留"))
            .default(false)
            .interact()?;
    if confirmed {
        let stream = client.remove(name).await?;
        write_operation_stream(stream).await
    } else {
        write_line("已取消")
    }
}

/// 执行 `config` 子命令。
async fn run_config(client: &mut RpcClient, args: super::args::ConfigArgs) -> anyhow::Result<()> {
    match args.command {
        super::args::ConfigCommand::Show => {
            let status = client.status().await?;
            write_line(&format!(
                "domain = {:?}\nstacks_root = {:?}\nlisten = {:?}",
                status.domain, status.stacks_root, status.listen
            ))?;
        }
        super::args::ConfigCommand::Set(super::args::ConfigSetArgs {
            item: super::args::ConfigItem::Domain(args),
        }) => {
            let response = client.set_domain(args.domain).await?;
            write_line(&response.message)?;
            if response.restart_required {
                match std::process::Command::new("systemctl")
                    .args(["restart", "nsetup.service"])
                    .status()
                {
                    Ok(status) if status.success() => {
                        write_line("daemon 已重启，新域名对后续命令生效")?;
                    }
                    Ok(status) => {
                        write_diagnostic(&format!(
                            "无法重启 daemon（退出码 {:?}），请手工执行 sudo systemctl restart nsetup.service",
                            status.code()
                        ))?;
                    }
                    Err(error) => {
                        write_diagnostic(&format!(
                            "无法执行 systemctl（{error}），请手工执行 sudo systemctl restart nsetup.service"
                        ))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// 从 Compose YAML 渲染解析后的 Traefik 路由表。
///
/// `project` 是项目名：nsetup 生成的 router 名固定为
/// `nsetup-<项目>-<服务>-<路由>`，不带项目名无法把它们从 label 中还原出来。
/// 表格同时包含模板生成的路由（来源 `nsetup`）和用户手写 label 的路由（来源
/// `labels`），并给出 host、path、entrypoint、scheme、priority、backend 与中间件。
///
/// # 错误
///
/// Compose YAML 或 Traefik label 无效时返回错误。
fn render_routes(project: &str, compose_yaml: &str) -> anyhow::Result<String> {
    let document: crate::spec::Document =
        serde_yaml::from_str(compose_yaml).context("无法解析项目 Compose YAML")?;
    let mut rows = Vec::new();
    for (service_name, service) in &document.services {
        for route in service.routes(project, service_name)? {
            for host in &route.hosts {
                rows.push(RouteRow {
                    host: host.clone(),
                    path: route.path_prefix.clone().unwrap_or_default(),
                    entrypoint: route.entrypoint_name().to_string(),
                    protocol: route.protocol.as_str().to_string(),
                    backend: route
                        .service
                        .clone()
                        .unwrap_or_else(|| service_name.clone()),
                    port: route.container_port,
                    middlewares: route.middleware_references(),
                    priority: route.priority,
                    managed: true,
                });
            }
        }
        for row in user_route_rows(service_name, service)? {
            rows.push(row);
        }
    }
    if rows.is_empty() {
        return Ok(String::from("该项目没有 Traefik 路由\n"));
    }
    rows.sort_by(|left, right| {
        (&left.host, &left.path, &right.priority, &left.entrypoint).cmp(&(
            &right.host,
            &right.path,
            &left.priority,
            &right.entrypoint,
        ))
    });
    let mut output = String::from("最终生效的 Traefik 路由\n");
    output.push_str("HOST\tPATH\tENTRYPOINT\tSCHEME\tPRIORITY\tBACKEND\tMIDDLEWARES\t来源\n");
    for row in rows {
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            row.host,
            if row.path.is_empty() { "/" } else { &row.path },
            row.entrypoint,
            row.protocol,
            row.priority
                .map_or_else(|| String::from("-"), |value| value.to_string()),
            match row.port {
                Some(port) => format!("{}:{port}", row.backend),
                None => row.backend.clone(),
            },
            if row.middlewares.is_empty() {
                String::from("-")
            } else {
                row.middlewares.join(",")
            },
            if row.managed { "nsetup" } else { "labels" }
        ));
    }
    Ok(output)
}

/// 把用户手写 label 声明的 router 还原为路由表行。
///
/// # 错误
///
/// label 不是合法 `KEY=VALUE` 列表时返回错误。
fn user_route_rows(
    service_name: &str,
    service: &crate::spec::Service,
) -> anyhow::Result<Vec<RouteRow>> {
    let mut rows = Vec::new();
    for route in service.user_routes()? {
        for host in &route.hosts {
            rows.push(RouteRow {
                host: host.clone(),
                path: route.path_prefix.clone().unwrap_or_default(),
                entrypoint: route.entrypoint.clone(),
                protocol: route.protocol.as_str().to_string(),
                backend: service_name.to_string(),
                port: route.container_port,
                middlewares: route.middlewares.clone(),
                priority: route.priority,
                managed: false,
            });
        }
    }
    Ok(rows)
}

/// 路由表的一行。
#[derive(Debug)]
struct RouteRow {
    /// 匹配的主机名。
    host: String,
    /// 路径前缀；空表示整个主机。
    path: String,
    /// 监听的 entrypoint。
    entrypoint: String,
    /// 后端协议。
    protocol: String,
    /// 后端名称：容器服务名或内置服务名（`api@internal`）。
    backend: String,
    /// 后端容器端口；内置服务与用户 label 路由未知时为 `None`。
    port: Option<u16>,
    /// 引用的中间件。
    middlewares: Vec<String>,
    /// 显式优先级。
    priority: Option<u32>,
    /// 是否由 nsetup 生成。
    managed: bool,
}

/// 显示流式变更阶段，并仅把最终结果写入 stdout。
async fn write_operation_stream(
    mut stream: tonic::Streaming<proto::OperationProgress>,
) -> anyhow::Result<()> {
    let mut completed = false;
    while let Some(progress) = stream.next().await.transpose()? {
        match proto::OperationStage::try_from(progress.stage)? {
            proto::OperationStage::Unspecified => {
                anyhow::bail!("daemon 返回了未指定的操作阶段");
            }
            proto::OperationStage::Queued | proto::OperationStage::Running => {
                write_diagnostic(&progress.message)?;
            }
            proto::OperationStage::Completed => {
                write_line(&progress.message)?;
                completed = true;
            }
        }
    }
    if !completed {
        anyhow::bail!("daemon 未返回操作完成阶段");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::render_routes;

    /// `show --routes` 必须列出 nsetup 生成的路由，并区分方案与优先级（R3）。
    #[test]
    fn routes_table_lists_generated_routes() -> anyhow::Result<()> {
        let config = crate::config::Config {
            stacks_root: std::path::PathBuf::from("/srv/nsetup/stacks"),
            ..crate::config::Config::default()
        };
        let input = r#"
format = 1
name = "netbird"
[services.server]
image = "netbirdio/netbird"
version = "0.50.0"
port = 80
[services.server.traefik]
[services.server.traefik.routes.grpc]
hosts = ["netbird"]
path_prefix = "/signalexchange.SignalExchange"
port = 10000
protocol = "h2c"
priority = 200
[services.dashboard]
image = "netbirdio/dashboard"
version = "2.0"
port = 80
labels = ["traefik.http.routers.custom.rule=Host(`extra.example.com`)", "traefik.http.services.custom.loadbalancer.server.port=8080"]
[services.dashboard.traefik]
hosts = ["netbird"]
path_prefix = "/"
priority = 1
"#;
        let generated = crate::template::apply(input, &config, None)?;
        let table = render_routes(&generated.spec.name, &generated.spec.compose_yaml()?)?;
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines[0], "最终生效的 Traefik 路由");
        let generated_row = lines
            .iter()
            .find(|line| line.contains("/signalexchange.SignalExchange"))
            .ok_or_else(|| anyhow::anyhow!("缺少 nsetup 生成的路由: {table}"))?;
        assert!(generated_row.contains("h2c"), "{generated_row}");
        assert!(generated_row.contains("\t200\t"), "{generated_row}");
        assert!(generated_row.contains("server:10000"), "{generated_row}");
        assert!(generated_row.ends_with("\tnsetup"), "{generated_row}");
        let label_row = lines
            .iter()
            .find(|line| line.starts_with("extra.example.com"))
            .ok_or_else(|| anyhow::anyhow!("缺少 label 路由: {table}"))?;
        assert!(label_row.contains("dashboard:8080"), "{label_row}");
        assert!(label_row.ends_with("\tlabels"), "{label_row}");
        Ok(())
    }

    /// 没有路由的项目仍给出明确结论。
    #[test]
    fn routes_table_reports_empty_projects() -> anyhow::Result<()> {
        let compose = "services:\n  web:\n    image: example/web:1\n";
        assert_eq!(
            render_routes("plain", compose)?,
            "该项目没有 Traefik 路由\n"
        );
        Ok(())
    }
}
