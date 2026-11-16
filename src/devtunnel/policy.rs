use std::path::PathBuf;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;

use crate::herdr::config::Connection;
use crate::process::output;
fn tunnel_value(value: &Value) -> &Value {
    value.get("tunnel").unwrap_or(value)
}

pub(crate) fn tunnel_id(value: &Value, fallback: &str) -> String {
    let t = tunnel_value(value);
    match (t["tunnelId"].as_str(), t["clusterId"].as_str()) {
        (Some(id), _) if id.contains('.') => id.into(),
        (Some(id), Some(cluster)) => format!("{id}.{cluster}"),
        _ => fallback.into(),
    }
}

pub(crate) fn empty_acl(value: &Value) -> Result<bool> {
    match value {
        Value::Array(entries) => Ok(entries.is_empty()),
        Value::Object(object) => match object.get("entries") {
            Some(Value::Array(entries)) => Ok(entries.is_empty()),
            _ => bail!("Unrecognized tunnel ACL format"),
        },
        _ => bail!("Unrecognized tunnel ACL format"),
    }
}

pub(crate) fn validate_ports(ports: &[Value], port: u16) -> Result<()> {
    ensure!(
        ports.len() <= 1
            && ports
                .iter()
                .all(|p| p["portNumber"].as_u64() == Some(u64::from(port))),
        "Use a dedicated tunnel with only port {port}"
    );
    for acl in ports
        .iter()
        .filter_map(|port| port.get("accessControl"))
        .filter(|acl| !acl.is_null())
    {
        ensure!(
            empty_acl(acl)?,
            "An owner-only private tunnel with no additional tunnel/port access grants is required"
        );
    }
    Ok(())
}

pub(crate) fn validate_tunnel(value: &Value, port: u16) -> Result<()> {
    let t = tunnel_value(value);
    match t.get("ports") {
        Some(Value::Array(ports)) => validate_ports(ports, port)?,
        None | Some(Value::Null) if t["tunnelId"].as_str().is_some() => {}
        _ => bail!("Tunnel has no recognizable ports array"),
    }
    if let Some(acl) = t.get("accessControl").filter(|acl| !acl.is_null()) {
        ensure!(
            empty_acl(acl)?,
            "An owner-only private tunnel with no additional tunnel/port access grants is required"
        );
    }
    Ok(())
}

pub(crate) fn port_list(value: &Value) -> Result<&[Value]> {
    let object = value.as_object().context("Unrecognized tunnel port list")?;
    match (object.get("ports"), object.get("warning"), object.len()) {
        (Some(Value::Array(ports)), None, 1) => Ok(ports),
        (None, Some(Value::String(warning)), 1) if warning == "No ports found for tunnel." => {
            Ok(&[])
        }
        _ => bail!("Unrecognized tunnel port list"),
    }
}

async fn validate_policy(c: &Connection, dt: &PathBuf, port: Option<u16>) -> Result<()> {
    let number = port.map(|port| port.to_string());
    let mut args = vec!["access", "list", &c.tunnel];
    if let Some(number) = &number {
        args.extend(["--port-number", number]);
    }
    args.push("--json");
    let response = output(dt, &args, c.timeout)
        .await
        .context("Failed to retrieve tunnel access policy")?;
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct AccessPolicy {
        access_control_entries: Vec<Value>,
        tunnel_id: Option<String>,
        port_number: Option<u16>,
    }

    let _: Value = serde_json::from_str(&response).context("Invalid access policy JSON")?;
    let policy: AccessPolicy = serde_json::from_str(&response).context(
        "Unrecognized access policy: missing accessControlEntries array or unexpected fields",
    )?;
    ensure!(
        policy
            .tunnel_id
            .as_ref()
            .is_none_or(|id| id.eq_ignore_ascii_case(&c.tunnel))
            && policy.port_number.is_none_or(|number| Some(number) == port),
        "Access policy does not match the requested tunnel or port"
    );
    ensure!(
        policy.access_control_entries.is_empty(),
        "An owner-only private tunnel with no additional tunnel/port access grants is required"
    );
    Ok(())
}

pub(crate) async fn validate_private_tunnel(
    c: &Connection,
    dt: &PathBuf,
    detail: &Value,
) -> Result<bool> {
    validate_tunnel(detail, c.port)?;
    let response = output(dt, &["port", "list", &c.tunnel, "--json"], c.timeout)
        .await
        .context("Failed to retrieve tunnel port list")?;
    let list: Value = serde_json::from_str(&response).context("Invalid tunnel port list JSON")?;
    let ports = port_list(&list)?;
    validate_ports(ports, c.port)?;
    let has_port = !ports.is_empty();
    ensure!(
        has_port
            || !tunnel_value(detail)["ports"]
                .as_array()
                .is_some_and(|ports| !ports.is_empty()),
        "Tunnel details and port list disagree"
    );
    validate_policy(c, dt, None).await?;
    if has_port {
        validate_policy(c, dt, Some(c.port)).await?;
    }
    Ok(has_port)
}
