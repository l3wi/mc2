import type { ReactNode } from "react";
import {
  ArrowUpRight,
  Lightbulb,
  Info,
  TriangleAlert,
  ShieldAlert,
  ChevronDown,
} from "lucide-react";

/* ---------- Callouts ---------- */

const calloutStyles = {
  tip: {
    icon: Lightbulb,
    box: "border-emerald-300/60 bg-emerald-50/60 dark:border-emerald-800/60 dark:bg-emerald-950/30",
    accent: "text-emerald-600 dark:text-emerald-400",
    label: "Tip",
  },
  note: {
    icon: Info,
    box: "border-sky-300/60 bg-sky-50/60 dark:border-sky-800/60 dark:bg-sky-950/30",
    accent: "text-sky-600 dark:text-sky-400",
    label: "Note",
  },
  warning: {
    icon: TriangleAlert,
    box: "border-amber-300/60 bg-amber-50/60 dark:border-amber-800/60 dark:bg-amber-950/30",
    accent: "text-amber-600 dark:text-amber-400",
    label: "Warning",
  },
  danger: {
    icon: ShieldAlert,
    box: "border-red-300/60 bg-red-50/60 dark:border-red-800/60 dark:bg-red-950/30",
    accent: "text-red-600 dark:text-red-400",
    label: "Danger",
  },
} as const;

export function Callout({
  type = "note",
  title,
  children,
}: {
  type?: keyof typeof calloutStyles;
  title?: string;
  children: ReactNode;
}) {
  const style = calloutStyles[type];
  const Icon = style.icon;
  return (
    <div className={`my-6 flex gap-3 rounded-xl border px-4 py-3 text-sm ${style.box}`}>
      <Icon className={`mt-0.5 h-4 w-4 shrink-0 ${style.accent}`} />
      <div className="min-w-0">
        {title && (
          <p className={`mb-1 text-xs font-semibold uppercase tracking-wider ${style.accent}`}>
            {title}
          </p>
        )}
        <div className="not-prose">{children}</div>
      </div>
    </div>
  );
}

export const Tip = (props: Omit<React.ComponentProps<typeof Callout>, "type">) => (
  <Callout {...props} type="tip" />
);
export const Note = (props: Omit<React.ComponentProps<typeof Callout>, "type">) => (
  <Callout {...props} type="note" />
);
export const Warning = (props: Omit<React.ComponentProps<typeof Callout>, "type">) => (
  <Callout {...props} type="warning" />
);
export const Danger = (props: Omit<React.ComponentProps<typeof Callout>, "type">) => (
  <Callout {...props} type="danger" />
);

/* ---------- Steps ---------- */

export function Steps({ children }: { children: ReactNode }) {
  return <ol className="mc2-steps my-6 space-y-5">{children}</ol>;
}

export function Step({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  return (
    <li className="mc2-step relative pl-12">
      <span className="absolute left-0 top-0 flex h-8 w-8 items-center justify-center rounded-lg border border-zinc-200 bg-zinc-50 font-mono text-sm font-semibold text-zinc-600 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-300" />
      <h3 className="mt-0 mb-2 scroll-mt-24 text-sm font-semibold">{title}</h3>
      <div className="not-prose space-y-3">{children}</div>
    </li>
  );
}

/* ---------- Cards ---------- */

export function Cards({
  cols = 2,
  children,
}: {
  cols?: 1 | 2 | 3 | 4;
  children: ReactNode;
}) {
  const grid = {
    1: "grid-cols-1",
    2: "grid-cols-1 sm:grid-cols-2",
    3: "grid-cols-1 sm:grid-cols-2 lg:grid-cols-3",
    4: "grid-cols-1 sm:grid-cols-2 lg:grid-cols-4",
  }[cols];
  return <div className={`not-prose my-6 grid gap-3 ${grid}`}>{children}</div>;
}

export function Card({
  title,
  description,
  href,
  icon,
}: {
  title: string;
  description?: string;
  href: string;
  icon?: ReactNode;
}) {
  return (
    <a
      href={href}
      className="group flex flex-col gap-1 rounded-xl border border-zinc-200 bg-white p-4 text-sm shadow-sm transition hover:border-accent/50 hover:shadow-md dark:border-zinc-800 dark:bg-zinc-900"
    >
      <span className="flex items-center gap-2 font-semibold text-zinc-900 dark:text-zinc-100">
        {icon && <span className="text-accent">{icon}</span>}
        {title}
        <ArrowUpRight className="ml-auto h-3.5 w-3.5 opacity-40 transition group-hover:opacity-100" />
      </span>
      {description && (
        <span className="text-zinc-500 dark:text-zinc-400">{description}</span>
      )}
    </a>
  );
}

/* ---------- Accordion ---------- */

export function Accordion({
  title,
  children,
  defaultOpen = false,
}: {
  title: string;
  children: ReactNode;
  defaultOpen?: boolean;
}) {
  return (
    <details
      open={defaultOpen}
      className="not-prose my-4 rounded-xl border border-zinc-200 dark:border-zinc-800"
    >
      <summary className="flex cursor-pointer list-none items-center gap-2 px-4 py-3 text-sm font-semibold [&::-webkit-details-marker]:hidden">
        <ChevronDown className="h-4 w-4 transition group-open:rotate-180" />
        {title}
      </summary>
      <div className="border-t border-zinc-200 px-4 py-3 text-sm dark:border-zinc-800">
        {children}
      </div>
    </details>
  );
}

/* ---------- MDX component map ---------- */

export const mdxComponents = {
  Callout,
  Tip,
  Note,
  Warning,
  Danger,
  Steps,
  Step,
  Cards,
  Card,
  Accordion,
};
