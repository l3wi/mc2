use microsandbox::{set_default_backend, LocalBackend, Sandbox};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let name = std::env::args().nth(1).expect("name");
    let local = LocalBackend::new().await?;
    set_default_backend(local);
    if let Ok(h) = Sandbox::get(&name).await {
        let _ = h.stop().await;
    }
    let _ = Sandbox::remove(&name).await;
    println!("removed {name}");
    Ok(())
}
