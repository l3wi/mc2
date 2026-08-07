//! Lab helper: msb_shell <name> <cmd...>
use microsandbox::{set_default_backend, LocalBackend, Sandbox};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let name = args.next().expect("sandbox name");
    let cmd: Vec<String> = args.collect();
    if cmd.is_empty() {
        anyhow::bail!("usage: msb_shell <name> <shell command...>");
    }
    let local = LocalBackend::new().await?;
    set_default_backend(local);
    let handle = Sandbox::get(&name).await?;
    let sb = handle.connect().await?;
    let out = sb.shell(cmd.join(" ")).await?;
    print!("{}", out.stdout().unwrap_or_default());
    eprint!("{}", out.stderr().unwrap_or_default());
    if !out.status().success {
        std::process::exit(out.status().code);
    }
    Ok(())
}
