//! Volume commands: `mc2 volume ls`.

use crate::cli::{ListArgs, OutputFormat};
use crate::client::{api_error, operator_get};
use crate::context::Conn;
use anyhow::{Context, Result};

pub(crate) async fn volume_ls(args: ListArgs, conn: &Conn) -> Result<()> {
    let url = format!("{}/v1/volumes", conn.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let res = operator_get(&client, &url, conn.token.as_deref())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error("volume ls", status, &body));
    }
    if matches!(args.output, OutputFormat::Json) {
        println!("{body}");
        return Ok(());
    }
    let views: Vec<mc2_api::VolumeView> =
        serde_json::from_str(&body).with_context(|| format!("parse: {body}"))?;
    if views.is_empty() {
        println!("No volumes.");
        return Ok(());
    }
    let mut t = crate::table::Table::new()
        .header(["STACK", "VOLUME", "SIZE", "PATH"])
        .right_align([2]);
    for v in views {
        t = t.row([v.stack, v.volume, format!("{} MiB", v.size_mib), v.path]);
    }
    print!("{}", t.render());
    Ok(())
}
