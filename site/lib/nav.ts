export type NavPage = {
  path: string;
  title: string;
  description?: string;
  /** Hide the page from the sidebar but keep it addressable. */
  hidden?: boolean;
};

export type NavGroup = {
  title: string;
  icon?: string;
  /** Default-collapse the group in the sidebar. */
  collapsed?: boolean;
  pages: NavPage[];
};

export type NavTab = {
  name: string;
  path: string;
  short: string;
  groups: NavGroup[];
};

export const tabs: NavTab[] = [
  {
    name: "Documentation",
    path: "/documentation",
    short: "Docs",
    groups: [
      {
        title: "Getting Started",
        icon: "rocket",
        pages: [
          { path: "introduction", title: "Introduction" },
          { path: "quickstart", title: "Quickstart" },
        ],
      },
      {
        title: "Concepts",
        icon: "lightbulb",
        pages: [
          {
            path: "concepts/architecture",
            title: "Architecture",
            description: "One process: REST, SQLite, scheduler, reconcile loop.",
          },
          {
            path: "concepts/desired-state",
            title: "Desired state & reconciliation",
            description: "Write what you want; MC2 makes it true and keeps it true.",
          },
          {
            path: "concepts/stacks",
            title: "Stacks",
            description: "The Compose-shaped desired-state document.",
          },
          {
            path: "concepts/services-and-instances",
            title: "Services & instances",
            description: "Replicas, restart policies, health, and ordering.",
          },
          {
            path: "concepts/networking",
            title: "Networking",
            description: "Host ports, service networks, and ingress.",
          },
          {
            path: "concepts/secrets",
            title: "Secrets",
            description: "Encrypted at rest, host-gated at runtime.",
          },
          {
            path: "concepts/storage",
            title: "Storage",
            description: "Named volumes that outlive a microVM.",
          },
          {
            path: "concepts/identity-and-contexts",
            title: "Identity & contexts",
            description: "Tokens, local vs remote, and connection resolution.",
          },
          {
            path: "concepts/observability",
            title: "Observability",
            description: "OTLP metrics and the msb-metrics sidecar.",
          },
          {
            path: "concepts/glossary",
            title: "Glossary",
            description: "Every MC2 term, defined once.",
          },
        ],
      },
      {
        title: "Guides",
        icon: "book-open",
        pages: [
          { path: "guides/run-your-first-stack", title: "Run your first stack" },
          { path: "guides/scale-and-restart", title: "Scale & restart" },
          { path: "guides/connect-services", title: "Connect services" },
          { path: "guides/publish-with-ingress", title: "Publish with ingress" },
          { path: "guides/add-secrets", title: "Add secrets" },
          { path: "guides/open-ssh", title: "Open SSH" },
          { path: "guides/use-persistent-volumes", title: "Use persistent volumes" },
          { path: "guides/order-startup", title: "Order startup" },
          { path: "guides/manage-a-remote-server", title: "Manage a remote server" },
          { path: "guides/tear-down-and-clean-up", title: "Tear down & clean up" },
        ],
      },
      {
        title: "Operations",
        icon: "server",
        pages: [
          { path: "operations/run-the-server", title: "Run the server" },
          { path: "operations/upgrade-and-migrate", title: "Upgrade & migrate" },
          { path: "operations/troubleshooting", title: "Troubleshooting" },
          { path: "operations/development", title: "Development & contributing" },
        ],
      },
    ],
  },
  {
    name: "References",
    path: "/references",
    short: "References",
    groups: [
      {
        title: "Stack YAML",
        icon: "file-code",
        pages: [
          { path: "stack/overview", title: "Overview" },
          { path: "stack/services", title: "Services" },
          { path: "stack/ports-expose", title: "Ports & expose" },
          { path: "stack/networks-ingress", title: "Networks & ingress" },
          { path: "stack/secrets-volumes", title: "Secrets & volumes" },
          { path: "stack/health-depends", title: "Healthchecks & depends_on" },
          { path: "stack/ssh-node", title: "SSH & node placement" },
        ],
      },
      {
        title: "CLI",
        icon: "terminal",
        collapsed: true,
        pages: [
          { path: "cli/overview", title: "Overview" },
          { path: "cli/stacks", title: "Stack commands" },
          { path: "cli/observe", title: "Observe commands" },
          { path: "cli/access", title: "Access commands" },
          { path: "cli/security", title: "Security commands" },
          { path: "cli/operate", title: "Operate commands" },
        ],
      },
      {
        title: "REST API",
        icon: "api",
        collapsed: true,
        pages: [
          { path: "api/overview", title: "Overview" },
          { path: "api/status", title: "Status" },
          { path: "api/stacks-instances", title: "Stacks & instances" },
          { path: "api/networking-ingress", title: "Networking & ingress" },
          { path: "api/secrets-ssh", title: "Secrets & SSH" },
        ],
      },
      {
        title: "Configuration",
        icon: "gear",
        collapsed: true,
        pages: [
          { path: "config/environment", title: "Environment variables" },
          { path: "config/contexts-file", title: "Contexts file" },
        ],
      },
    ],
  },
  {
    name: "Recipes",
    path: "/recipes",
    short: "Recipes",
    groups: [
      {
        title: "Scenarios",
        icon: "flask-conical",
        pages: [
          { path: "agent-swarm", title: "Agent swarm" },
          { path: "compose-to-mc2", title: "Migrate from Docker Compose" },
          { path: "remote-ssh-agents", title: "Remote coding agents over SSH" },
          { path: "throwaway-test-grid", title: "Throwaway test grid" },
          { path: "headless-browser-fleet", title: "Headless browser fleet" },
          { path: "shared-network-multi-stack", title: "Shared networks across stacks" },
          { path: "metrics-backends", title: "Metrics backends" },
        ],
      },
    ],
  },
  {
    name: "Security",
    path: "/security",
    short: "Security",
    groups: [
      {
        title: "Security",
        icon: "shield-half",
        pages: [
          { path: "overview", title: "Overview" },
          { path: "isolation", title: "Isolation" },
          { path: "secrets", title: "Secrets" },
          { path: "networking", title: "Networking" },
          { path: "authentication", title: "Authentication" },
          { path: "hardening", title: "Hardening checklist" },
        ],
      },
    ],
  },
  {
    name: "Changelog",
    path: "/changelog",
    short: "Changelog",
    groups: [
      {
        title: "Releases",
        icon: "clock",
        pages: [
          { path: "unreleased", title: "Unreleased" },
          { path: "0.1.0", title: "0.1.0" },
        ],
      },
    ],
  },
];

export function findTab(pathname: string): NavTab | undefined {
  return tabs.find((t) => pathname === t.path || pathname.startsWith(t.path + "/"));
}

/** Flatten every (tab, pagePath) pair for static generation. */
export function allPages(): { tab: NavTab; page: NavPage }[] {
  const out: { tab: NavTab; page: NavPage }[] = [];
  for (const tab of tabs) {
    for (const group of tab.groups) {
      for (const page of group.pages) {
        out.push({ tab, page });
      }
    }
  }
  return out;
}

/** Ordered, visible page list for one tab (used for prev/next navigation). */
export function orderedPages(tab: NavTab): { page: NavPage; href: string }[] {
  const out: { page: NavPage; href: string }[] = [];
  for (const group of tab.groups) {
    for (const page of group.pages) {
      if (!page.hidden) out.push({ page, href: `${tab.path}/${page.path}` });
    }
  }
  return out;
}
