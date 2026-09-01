//! `spec` 模块行为测试。

use super::project::{parse_env_file, serialize_env_file};
use super::{Document, Route, RouteProtocol, Service, StackSpec};
use std::collections::BTreeMap;

#[test]
fn compose_unknown_field_is_rejected() {
    let result = StackSpec::parse(
        "demo",
        "services:\n  app:\n    image: example/app:1\n    depends_on: [db]\n",
        "",
    );
    assert!(result.is_err());
}

#[test]
fn routes_round_trip_through_labels() -> anyhow::Result<()> {
    let mut service = Service {
        image: String::from("example/app:1"),
        ..Service::default()
    };
    let expected = Route {
        name: String::from("default"),
        hosts: vec![String::from("app.example.com")],
        path_prefix: Some(String::from("/api")),
        container_port: 8080,
        middlewares: vec![String::from("gzip")],
        protocol: RouteProtocol::H2c,
        sticky_cookie: true,
        pass_host_header: Some(false),
        priority: Some(100),
    };
    service.set_routes("demo", "web", std::slice::from_ref(&expected))?;
    assert!(service.labels.iter().any(|label| {
        label == "traefik.http.routers.nsetup-demo-web-default.tls.certresolver=cloudflare"
    }));
    assert!(service.labels.iter().any(|label| {
        label == "traefik.http.routers.nsetup-demo-web-default.entrypoints=https"
    }));
    assert_eq!(service.routes("demo", "web")?, vec![expected]);
    let spec = StackSpec {
        name: String::from("demo"),
        document: Document {
            services: BTreeMap::from([(String::from("web"), service)]),
            networks: BTreeMap::new(),
        },
        environment: BTreeMap::new(),
    };
    spec.validate()?;
    Ok(())
}

/// 镜像钉版本同时拒绝缺少标签和可变的 `latest` 标签。
#[test]
fn rejects_unpinned_images() {
    for image in ["example/app", "example/app:latest"] {
        let compose = format!("services:\n  app:\n    image: {image}\n");
        assert!(StackSpec::parse("demo", &compose, "").is_err());
    }
}

/// 容器环境文件不能越出受管项目目录。
#[test]
fn rejects_escaping_env_file() {
    let compose = "services:\n  app:\n    image: example/app:1\n    env_file: [../secret]\n";
    assert!(StackSpec::parse("demo", compose, "").is_err());
}

/// 受管 `.env` 转义应保留空格、引号、换行符和字面量美元符号。
#[test]
fn env_file_round_trip() -> anyhow::Result<()> {
    let parsed = parse_env_file(
        "A=plain\nB=\"two words\"\nC=\"line\\nnext\"\nD='$argon2id$v=19'\nE='owner\\'s $value'\n",
    )?;
    let normalized = serialize_env_file(&parsed);
    assert!(normalized.contains("D='$argon2id$v=19'"));
    assert!(normalized.contains("E='owner\\'s $value'"));
    assert_eq!(parse_env_file(&normalized)?, parsed);
    Ok(())
}

/// 由变量支撑的模板镜像不能按字面镜像标签编辑。
#[test]
fn rejects_version_edit_for_variable_image() {
    let mut service = Service {
        image: String::from("nginx:${NGINX_VERSION}"),
        ..Service::default()
    };
    assert!(service.set_image_version(None, Some("1.28")).is_err());
}
