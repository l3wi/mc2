//! Grouped top-level help (`mc2` / `mc2 --help`): clap has no native subcommand
//! grouping, so the default "Commands:" block is spliced out and re-rendered
//! under the same visual groups Docker and the msb CLI use. Subcommand help
//! (e.g. `mc2 up --help`) is untouched and stays clap-native.

use crate::cli::Cli;
use clap::CommandFactory;
use std::io::IsTerminal;

/// A visual group for top-level command help.
struct CommandGroup {
    heading: &'static str,
    commands: &'static [&'static str],
}

const TOP_LEVEL_COMMAND_GROUPS: &[CommandGroup] = &[
    CommandGroup {
        heading: "Stacks",
        commands: &["up", "down", "config", "ps"],
    },
    CommandGroup {
        heading: "Observe",
        commands: &["logs", "status", "network", "ingress"],
    },
    CommandGroup {
        heading: "Access",
        commands: &["exec", "ssh"],
    },
    CommandGroup {
        heading: "Security",
        commands: &["secret", "context"],
    },
    CommandGroup {
        heading: "Admin",
        commands: &[
            "server",
            "doctor",
            "node",
            "setup",
            "completions",
            "version",
        ],
    },
];

/// Rendered help text for one top-level command.
#[derive(Clone)]
struct CommandHelpLine {
    name: String,
    help: String,
}

/// ANSI styling state for the custom top-level help.
struct HelpStyles {
    enabled: bool,
}

impl HelpStyles {
    /// Enable styling only on a TTY and when NO_COLOR is not set.
    fn detect() -> Self {
        Self {
            enabled: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    /// Style a group heading like clap's header style.
    fn header(&self, value: &str) -> String {
        if self.enabled {
            format!("\x1b[1;33m{value}\x1b[0m")
        } else {
            value.to_string()
        }
    }

    /// Style a command literal like clap's literal style.
    fn literal(&self, value: &str) -> String {
        if self.enabled {
            format!("\x1b[1;34m{value}\x1b[0m")
        } else {
            value.to_string()
        }
    }

    /// Style colon-heading lines (e.g. `Usage:`, `Options:`) in clap's style,
    /// preserving every line's trailing newline so spacing is untouched.
    fn section(&self, value: &str) -> String {
        value
            .split_inclusive('\n')
            .map(|line| {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.ends_with(':') {
                    format!("{}\n", self.header(trimmed))
                } else {
                    line.to_string()
                }
            })
            .collect()
    }
}

/// Return whether the current invocation asks only for top-level help
/// (bare `mc2`, or `mc2 -h` / `mc2 --help`).
fn is_top_level_help_request() -> bool {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        return true;
    }
    args.iter().all(|a| a == "-h" || a == "--help")
}

/// Print grouped top-level help for `mc2` and `mc2 --help`.
pub fn try_show_grouped_top_level_help() -> bool {
    if !is_top_level_help_request() {
        return false;
    }
    print!("{}", render_grouped_top_level_help());
    true
}

/// Render top-level help with visually grouped commands.
fn render_grouped_top_level_help() -> String {
    let mut cmd = Cli::command();
    let styles = HelpStyles::detect();
    let mut buf = Vec::new();
    cmd.write_long_help(&mut buf)
        .expect("clap help should render");
    let default_help = String::from_utf8(buf).expect("clap help should be valid UTF-8");
    let Some((prefix, _)) = default_help.split_once("\nCommands:\n") else {
        return default_help;
    };
    let Some((_, suffix)) = default_help.split_once("\nOptions:\n") else {
        return default_help;
    };

    let mut out = String::new();
    out.push_str(&styles.section(prefix));
    out.push('\n');
    out.push_str(&render_grouped_commands(&cmd, &styles));
    out.push('\n');
    out.push_str(&styles.header("Options:"));
    out.push('\n');
    out.push_str(&styles.section(suffix));
    out
}

/// Render top-level commands under the configured visual groups.
fn render_grouped_commands(cmd: &clap::Command, styles: &HelpStyles) -> String {
    let lines = visible_command_help_lines(cmd);
    let name_width = lines.iter().map(|l| l.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    let mut rendered: Vec<&str> = Vec::new();

    for (index, group) in TOP_LEVEL_COMMAND_GROUPS.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&styles.header(&format!("{}:", group.heading)));
        out.push('\n');
        for command in group.commands {
            if let Some(line) = lines.iter().find(|l| l.name == *command) {
                out.push_str(&format_command_help_line(line, name_width, styles));
                rendered.push(line.name.as_str());
            }
        }
    }

    let mut other: Vec<CommandHelpLine> = lines
        .iter()
        .filter(|l| !rendered.contains(&l.name.as_str()))
        .cloned()
        .collect();
    if !other.iter().any(|l| l.name == "help") {
        other.push(CommandHelpLine {
            name: "help".to_string(),
            help: "Print this message or the help of the given subcommand(s)".to_string(),
        });
    }

    out.push('\n');
    out.push_str(&styles.header("Other:"));
    out.push('\n');
    for line in &other {
        out.push_str(&format_command_help_line(line, name_width, styles));
    }
    out
}

/// Collect visible top-level commands from clap.
fn visible_command_help_lines(cmd: &clap::Command) -> Vec<CommandHelpLine> {
    cmd.get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| CommandHelpLine {
            name: c.get_name().to_string(),
            help: c.get_about().map(ToString::to_string).unwrap_or_default(),
        })
        .collect()
}

/// Format one command help line with clap-like spacing.
fn format_command_help_line(
    line: &CommandHelpLine,
    name_width: usize,
    styles: &HelpStyles,
) -> String {
    let padded = format!("{:<width$}", line.name, width = name_width);
    format!("  {} {}\n", styles.literal(&padded), line.help)
}
