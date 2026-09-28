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

    /// Operator API bearer token (the env value is never echoed in `--help`)
    #[arg(long, global = true, env = "MC2_API_KEY", hide_env_values = true)]
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
    Server(Box<ServerCmd>),
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

/// `mc2 server`: run the orchestrator, or operate on its data directory.
#[derive(Debug, Parser)]
pub struct ServerCmd {
    #[command(subcommand)]
    pub command: Option<ServerCommands>,

    #[command(flatten)]
    pub args: mc2_server::ServerArgs,
}

#[derive(Debug, Subcommand)]
pub enum ServerCommands {
    /// Operator API token operations (needs filesystem access to the data dir)
    Token(ServerTokenCmd),
    /// Stored-secret recovery (needs filesystem access to the data dir)
    Secrets(ServerSecretsCmd),
}

#[derive(Debug, Parser)]
pub struct ServerTokenCmd {
    #[command(subcommand)]
    pub command: ServerTokenCommands,
}

#[derive(Debug, Subcommand)]
pub enum ServerTokenCommands {
    /// Generate a new operator API token and replace the stored hash
    Rotate(TokenRotateArgs),
}

#[derive(Debug, Parser)]
pub struct TokenRotateArgs {
    /// Data directory (SQLite, tokens)
    #[arg(long, default_value = "~/.mc2", env = "MC2_DATA_DIR")]
    pub data_dir: String,
}

#[derive(Debug, Parser)]
pub struct ServerSecretsCmd {
    #[command(subcommand)]
    pub command: ServerSecretsCommands,
}

#[derive(Debug, Subcommand)]
pub enum ServerSecretsCommands {
    /// Delete every stored secret — the recovery path when the secrets key is
    /// lost (the server refuses to start while undecryptable secrets remain)
    Purge(SecretsPurgeArgs),
}

#[derive(Debug, Parser)]
pub struct SecretsPurgeArgs {
    /// Data directory (SQLite, tokens)
    #[arg(long, default_value = "~/.mc2", env = "MC2_DATA_DIR")]
    pub data_dir: String,
    /// Actually delete (without it, only list what would be deleted)
    #[arg(long)]
    pub yes: bool,
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
    /// Instance id, or `stack/service/ordinal` (e.g. `demo/web/0`)
    pub id: String,
}

#[derive(Debug, Parser)]
pub struct SshOpenArgs {
    /// Instance id, or `stack/service/ordinal` (e.g. `demo/web/0`)
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
    #[arg(long, env = "MC2_SECRET_VALUE", hide_env_values = true)]
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
    /// Path to the stack YAML file: a bare compose document with top-level
    /// `name:`, `services:`, and optional `volumes:` / `networks:` /
    /// `ingress:`.
    ///
    /// Kubernetes-style boilerplate (`apiVersion`, `kind`, `metadata`) is
    /// **not** accepted — like any other unknown top-level key it is rejected
    /// at parse. A missing `name:` is filled from the file stem — or, when the
    /// stem is the generic `stack` (e.g. `.../01-hello-service/stack.yaml`),
    /// from the parent directory name — sanitized to the lowercase stack-name
    /// grammar.
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
    /// Command and arguments to run in the sandbox.
    ///
    /// Everything after the instance ref is passed through verbatim — arguments
    /// starting with `-` (e.g. `-c`) included — so `mc2 exec ref /bin/sh -c 'x'`
    /// and `mc2 exec ref wget -qO- http://…` both work. `mc2`'s own global flags
    /// (`--token`, `--context`, …) are therefore only recognised *before* the
    /// instance ref. `--` still works as an explicit escape — and is required
    /// to forward a guest `--help`/`-h`/`-V`, which clap keeps for `mc2`
    /// itself.
    #[arg(
        required = true,
        num_args = 1..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
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

    #[test]
    fn server_token_rotate_parses() {
        let cli =
            Cli::try_parse_from(["mc2", "server", "token", "rotate", "--data-dir", "/tmp/mc2"])
                .unwrap();
        let Commands::Server(cmd) = cli.command else {
            panic!("expected server command");
        };
        match *cmd {
            ServerCmd {
                command:
                    Some(ServerCommands::Token(ServerTokenCmd {
                        command: ServerTokenCommands::Rotate(a),
                    })),
                ..
            } => assert_eq!(a.data_dir, "/tmp/mc2"),
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn plain_server_flags_still_parse() {
        let cli = Cli::try_parse_from([
            "mc2",
            "server",
            "--init-only",
            "--data-dir",
            "/tmp/mc2",
            "--no-auth",
            "--allow-unauthenticated-remote",
        ])
        .unwrap();
        let Commands::Server(cmd) = cli.command else {
            panic!("expected server command");
        };
        match *cmd {
            ServerCmd {
                command: None,
                args,
                ..
            } => {
                assert!(args.init_only);
                assert!(args.no_auth);
                assert!(args.allow_unauthenticated_remote);
                assert_eq!(args.data_dir, "/tmp/mc2");
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    /// Everything after the instance ref is a guest argv token — hyphen
    /// prefixed ones included — with no `--` required.
    #[test]
    fn exec_forwards_hyphen_args_after_the_instance_ref() {
        let cases: [(&[&str], &[&str]); 3] = [
            (
                &["mc2", "exec", "demo/web/0", "/bin/sh", "-c", "uname -a"],
                &["/bin/sh", "-c", "uname -a"],
            ),
            (
                &[
                    "mc2",
                    "exec",
                    "demo/web/0",
                    "wget",
                    "-qO-",
                    "http://echo.smoke-networks.svc.mc2:8080/",
                ],
                &["wget", "-qO-", "http://echo.smoke-networks.svc.mc2:8080/"],
            ),
            (
                &["mc2", "exec", "demo/web/0", "ls", "--", "-l"],
                &["ls", "--", "-l"],
            ),
        ];
        for (argv, expected) in cases {
            let cli = Cli::try_parse_from(argv).unwrap();
            let Commands::Exec(args) = cli.command else {
                panic!("expected exec command for {argv:?}");
            };
            assert_eq!(args.instance, "demo/web/0");
            assert_eq!(args.cmd, expected, "argv: {argv:?}");
        }
    }

    /// `--` explicitly escapes a guest argv that would otherwise look like a
    /// flag, and still forwards everything verbatim.
    #[test]
    fn exec_double_dash_form_works() {
        let cli =
            Cli::try_parse_from(["mc2", "exec", "demo/web/0", "--", "/bin/sh", "-c", "x"]).unwrap();
        let Commands::Exec(args) = cli.command else {
            panic!("expected exec command");
        };
        assert_eq!(args.instance, "demo/web/0");
        assert_eq!(args.cmd, ["/bin/sh", "-c", "x"]);
    }

    /// clap keeps `--help`/`-V` for `mc2` itself even after the instance ref
    /// (standard CLI behaviour), so a *guest* `--help` needs `--`.
    #[test]
    fn exec_forwards_a_guest_help_after_the_double_dash() {
        let cli = Cli::try_parse_from(["mc2", "exec", "demo/web/0", "--", "ls", "--help"]).unwrap();
        let Commands::Exec(args) = cli.command else {
            panic!("expected exec command");
        };
        assert_eq!(args.cmd, ["ls", "--help"]);
    }

    /// MC2's own global flags are recognised only before the instance ref; after
    /// it they belong to the guest command.
    #[test]
    fn exec_global_flags_only_before_the_instance_ref() {
        let cli = Cli::try_parse_from(["mc2", "--token", "mc2at_x", "exec", "demo/web/0", "env"])
            .unwrap();
        assert_eq!(cli.token.as_deref(), Some("mc2at_x"));
        let Commands::Exec(args) = cli.command else {
            panic!("expected exec command");
        };
        assert_eq!(args.cmd, ["env"]);

        // After the instance ref it is a guest token, not MC2's `--token`
        // (asserted via inequality so an ambient MC2_API_KEY cannot skew it).
        let cli = Cli::try_parse_from(["mc2", "exec", "demo/web/0", "printenv", "--token=leaked"])
            .unwrap();
        assert_ne!(cli.token.as_deref(), Some("leaked"));
        let Commands::Exec(args) = cli.command else {
            panic!("expected exec command");
        };
        assert_eq!(args.cmd, ["printenv", "--token=leaked"]);
    }

    /// A missing command is still a clap error.
    #[test]
    fn exec_requires_a_command() {
        assert!(Cli::try_parse_from(["mc2", "exec", "demo/web/0"]).is_err());
    }
}
