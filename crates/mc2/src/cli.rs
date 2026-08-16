//! clap command-line definitions for the `mc2` binary.

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "mc2",
    about = "MC2 (MicroCommandControl) — YAML-driven orchestration for microsandbox microVMs",
    long_about = "MC2 (MicroCommandControl) runs microsandbox microVMs from desired stack YAML. \
                  One binary: the orchestrator (server) and operator CLI.",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Control plane REST base URL (overrides MC2_API and the current context)
    #[arg(
        long,
        global = true,
        default_value = "",
        env = "MC2_API",
        hide_default_value = true
    )]
    pub api: String,

    /// Operator API bearer token
    #[arg(long, global = true, env = "MC2_API_KEY")]
    pub token: Option<String>,

    /// Named context from ~/.mc2/config.toml (overrides the current context)
    #[arg(long, global = true, env = "MC2_CONTEXT")]
    pub context: Option<String>,

    /// Allow plaintext http:// for a remote (non-loopback) control plane.
    /// Prefer https:// + TLS; the operator token travels unencrypted otherwise.
    #[arg(long, global = true)]
    pub allow_insecure_http: bool,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Bring up a stack: publish desired state and converge (idempotent)
    Up(UpArgs),
    /// Tear down a stack (instances + definition; volumes retained).
    /// `--volumes` also deletes named volumes; `rm` is an alias for `down`.
    #[command(alias = "rm")]
    Down(DownArgs),
    /// Validate and print a normalized stack config
    Config(ConfigArgs),
    /// List instances (desired sandboxes)
    Ps(PsArgs),

    /// Print recent sandbox logs for an instance
    Logs(LogsArgs),
    /// Cluster status (health + version + counts)
    Status(StatusArgs),
    /// Server-wide network membership summary (default + named networks)
    Network(NetworkArgs),
    /// Desired ingress routes
    Ingress(IngressArgs),
    /// Named volumes (retained across stack removal)
    Volume(VolumeCmd),

    /// Run a command inside an instance's sandbox
    Exec(ExecArgs),
    /// SSH authorized keys + endpoints
    Ssh(SshCmd),

    /// Secrets (encrypted at rest; values never listed)
    Secret(SecretCmd),
    /// Manage named API contexts (local/remote)
    Context(ContextCmd),

    /// Run the MC2 orchestrator (single process)
    Server(Box<mc2_server::ServerArgs>),
    /// Check host readiness (hypervisor / msb / paths)
    Doctor(DoctorArgs),
    /// Node operations
    Node(NodeCmd),
    /// Interactive setup wizard (server / client)
    Setup(SetupCmd),
    /// Generate shell completions
    Completions(CompletionsArgs),
    /// Show version info
    Version,
}

#[derive(Debug, Parser)]
pub struct SshCmd {
    #[command(subcommand)]
    pub command: SshCommands,
}

#[derive(Debug, Subcommand)]
pub enum SshCommands {
    /// Add or replace an authorized public key
    AddKey(SshKeyAddArgs),
    /// Show one authorized public key
    ShowKey(SshKeyShowArgs),
    /// List authorized public keys
    #[command(name = "keys", alias = "key-ls")]
    Keys(ListArgs),
    /// Remove an authorized public key
    RmKey(SshKeyRmArgs),
    /// List open SSH endpoints
    #[command(name = "ls", alias = "list")]
    Ls(ListArgs),
    /// Show SSH state for an instance
    Show(SshInstanceArgs),
    /// Open SSH on an instance (API override)
    Open(SshOpenArgs),
    /// Close SSH on an instance
    Close(SshInstanceArgs),
}

#[derive(Debug, Parser)]
pub struct SshKeyAddArgs {
    pub name: String,
    /// Public key line (or use --file)
    #[arg(long)]
    pub key: Option<String>,
    /// Path to .pub file
    #[arg(long)]
    pub file: Option<String>,
}

#[derive(Debug, Parser)]
pub struct SshKeyRmArgs {
    pub name: String,
}

#[derive(Debug, Parser)]
pub struct SshInstanceArgs {
    /// Instance id
    pub id: String,
}

#[derive(Debug, Parser)]
pub struct SshOpenArgs {
    pub id: String,
    /// Authorized key names (repeatable)
    #[arg(long = "key", required = true)]
    pub keys: Vec<String>,
    #[arg(long, default_value = "127.0.0.1")]
    pub bind: String,
    #[arg(long, default_value_t = 0)]
    pub port: u16,
}

#[derive(Debug, Parser)]
pub struct SecretCmd {
    #[command(subcommand)]
    pub command: SecretCommands,
}

#[derive(Debug, Subcommand)]
pub enum SecretCommands {
    /// Create or replace a secret value
    Set(SecretSetArgs),
    /// List secret names (values never shown)
    #[command(name = "ls", alias = "list")]
    Ls(ListArgs),
    #[command(name = "rm", alias = "delete")]
    Rm(SecretRmArgs),
}

#[derive(Debug, Parser)]
pub struct SecretSetArgs {
    /// Secret name
    pub name: String,
    /// Secret value (prefer env MC2_SECRET_VALUE or stdin for scripts)
    #[arg(long, env = "MC2_SECRET_VALUE")]
    pub value: Option<String>,
}

#[derive(Debug, Parser)]
pub struct SecretRmArgs {
    pub name: String,
}

#[derive(Debug, Parser)]
pub struct DoctorArgs {
    /// Also try `msb version`
    #[arg(long, default_value_t = true)]
    pub msb: bool,
}

#[derive(Debug, Parser)]
pub struct UpArgs {
    /// Path to stack YAML. Boilerplate is optional: apiVersion, kind, and
    /// metadata.name (defaults to the file name) are filled in when missing.
    #[arg(short = 'f', long = "file")]
    pub file: String,
}

#[derive(Debug, Parser)]
pub struct DownArgs {
    /// Stack name
    pub stack: String,
    /// Also delete the stack's named volumes (default: retained)
    #[arg(long)]
    pub volumes: bool,
}

#[derive(Debug, Parser)]
pub struct ConfigArgs {
    /// Path to stack YAML
    #[arg(short = 'f', long = "file")]
    pub file: String,
}

#[derive(Debug, Parser)]
pub struct ExecArgs {
    /// Instance id, or `stack/service/ordinal` (e.g. `demo/web/0`)
    pub instance: String,
    /// Command to run in the sandbox
    #[arg(required = true, num_args = 1..)]
    pub cmd: Vec<String>,
}

#[derive(Debug, Parser)]
pub struct LogsArgs {
    /// Instance id, or `stack/service/ordinal` (e.g. `demo/web/0`)
    pub instance: String,
    /// Show only the last N entries
    #[arg(long)]
    pub tail: Option<usize>,
    /// Keep streaming new entries as they arrive
    #[arg(long)]
    pub follow: bool,
}

#[derive(Debug, Parser)]
pub struct NodeCmd {
    #[command(subcommand)]
    pub command: NodeCommands,
}

#[derive(Debug, Subcommand)]
pub enum NodeCommands {
    /// List registered nodes (`mc2 node ls`)
    #[command(name = "ls", alias = "list")]
    Ls(ListArgs),
}

/// Output format for listing commands.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table (default)
    #[default]
    Table,
    /// Machine-readable JSON
    Json,
}

/// Common listing args: output format (connection is global).
#[derive(Debug, Parser)]
pub struct ListArgs {
    /// Output format
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    pub output: OutputFormat,
}

#[derive(Debug, Parser)]
pub struct StatusArgs {
    /// Output format
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    pub output: OutputFormat,
}

#[derive(Debug, Parser)]
pub struct PsArgs {
    /// Output format
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    pub output: OutputFormat,
    /// Only instances of this stack
    #[arg(long)]
    pub stack: Option<String>,
    /// Only instances of this service
    #[arg(long)]
    pub service: Option<String>,
}

#[derive(Debug, Parser)]
pub struct NetworkArgs {
    /// Network name, or `stack/service/ordinal` for per-instance connectivity (all networks when omitted)
    pub network: Option<String>,
    /// Output format
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    pub output: OutputFormat,
}

#[derive(Debug, Parser)]
pub struct VolumeCmd {
    #[command(subcommand)]
    pub command: VolumeCommands,
}

#[derive(Debug, Subcommand)]
pub enum VolumeCommands {
    /// List named volumes retained on the node
    #[command(name = "ls", alias = "list")]
    Ls(ListArgs),
}

#[derive(Debug, Parser)]
pub struct IngressArgs {
    /// Output format
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    pub output: OutputFormat,
}

#[derive(Debug, Parser)]
pub struct SshKeyShowArgs {
    pub name: String,
}

#[derive(Debug, Parser)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

#[derive(Debug, Parser)]
pub struct ContextCmd {
    #[command(subcommand)]
    pub command: ContextCommands,
}

#[derive(Debug, Subcommand)]
pub enum ContextCommands {
    /// List contexts (name, URL, mode) and the current one
    Ls,
    /// Switch the current context
    Use(UseContextArgs),
    /// Create or update a context (upsert)
    Set(SetContextArgs),
}

#[derive(Debug, Parser)]
pub struct UseContextArgs {
    /// Context name
    pub name: String,
}

#[derive(Debug, Parser)]
pub struct SetContextArgs {
    /// Context name
    pub name: String,
    /// Control plane REST base URL
    #[arg(long)]
    pub api: String,
    /// Operator API bearer token (stored in the 0600 config file)
    #[arg(long)]
    pub token: Option<String>,
}

#[derive(Debug, Parser)]
pub struct SetupCmd {
    #[command(subcommand)]
    pub command: Option<SetupCommands>,
}

#[derive(Debug, Subcommand)]
pub enum SetupCommands {
    /// Interactive server setup (run on the VPS)
    Server,
    /// Interactive client setup (connect to a server)
    Client,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_help() {
        Cli::command().debug_assert();
    }
}
