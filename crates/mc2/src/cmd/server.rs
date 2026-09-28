//! Server data-directory subcommands (`mc2 server token rotate`,
//! `mc2 server secrets purge`). Both open the SQLite DB directly (filesystem
//! authority), so they work whether or not the server is running.

use crate::cli::{SecretsPurgeArgs, TokenRotateArgs};
use anyhow::{bail, Context, Result};
use mc2_store::{SqliteStore, Store};
use std::sync::Arc;

/// Open an initialized data directory's store.
async fn open_data_dir(raw: &str) -> Result<Arc<SqliteStore>> {
    let data_dir = mc2_server::expand_data_dir(raw);
    let db_path = data_dir.join("mc2.db");
    if !db_path.exists() {
        bail!(
            "no MC2 data directory at {} — run `mc2 server` once to initialize it",
            data_dir.display()
        );
    }
    let store = SqliteStore::open(&db_path).await.context("open store")?;
    if store.get_cluster_meta().await?.is_none() {
        bail!(
            "{} is not an initialized MC2 data directory",
            data_dir.display()
        );
    }
    Ok(store)
}

/// Generate a new operator API token and replace the stored hash.
///
/// Tokens are verified against the store per request, so the old token stops
/// working immediately, even on a running server.
pub async fn token_rotate(args: TokenRotateArgs) -> Result<()> {
    let store = open_data_dir(&args.data_dir).await?;
    let token = mc2_server::rotate_api_token(store.as_ref()).await?;

    println!("New operator API token (shown once — save it now):");
    println!();
    println!("  {token}");
    println!();
    println!("Update your clients, e.g.:");
    println!("  mc2 context set <name> --api <url> --token {token}");
    println!();
    println!("The previous token stops working immediately.");
    Ok(())
}

/// Delete every stored secret: the recovery path when the secrets key is lost
/// (the server refuses to start while it cannot decrypt stored secrets).
/// Without `--yes` it only lists what would be deleted.
pub async fn secrets_purge(args: SecretsPurgeArgs) -> Result<()> {
    let store = open_data_dir(&args.data_dir).await?;
    let names: Vec<String> = store
        .list_secret_meta()
        .await?
        .into_iter()
        .map(|m| m.name)
        .collect();
    if names.is_empty() {
        println!("No stored secrets.");
        return Ok(());
    }
    if !args.yes {
        println!("Would delete {} stored secret(s):", names.len());
        for n in &names {
            println!("  {n}");
        }
        println!();
        println!("Re-run with --yes to delete them, then set them again with `mc2 secret set`.");
        return Ok(());
    }
    for n in &names {
        store
            .delete_secret(n)
            .await
            .with_context(|| format!("delete secret {n}"))?;
        println!("deleted {n}");
    }
    println!();
    println!(
        "Start the server (a new key is generated if the old one is missing), then set the \
         values again with `mc2 secret set <name>`."
    );
    Ok(())
}
