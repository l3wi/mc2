"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { tabs } from "@/lib/nav";
import { ThemeToggle } from "./theme-toggle";

function firstPage(tabPath: string): string {
  const tab = tabs.find((t) => t.path === tabPath);
  if (!tab) return tabPath;
  return `${tab.path}/${tab.groups[0].pages[0].path}`;
}

export function TopNav() {
  const pathname = usePathname();

  return (
    <header className="sticky top-0 z-40 border-b border-zinc-200 bg-white/85 backdrop-blur dark:border-zinc-800 dark:bg-zinc-950/85">
      <div className="mx-auto flex h-14 w-full max-w-[1200px] items-center gap-6 px-4">
        <Link
          href="/documentation/introduction"
          className="flex shrink-0 items-center gap-2 font-semibold tracking-tight"
        >
          <span className="flex h-6 w-6 items-center justify-center rounded-md bg-accent font-mono text-xs font-bold text-white">
            M
          </span>
          <span className="hidden sm:inline">MC2 Docs</span>
        </Link>

        <nav className="flex min-w-0 items-center gap-1 overflow-x-auto">
          {tabs.map((tab) => {
            const href = firstPage(tab.path);
            const active =
              pathname === tab.path || pathname.startsWith(tab.path + "/");
            return (
              <Link
                key={tab.path}
                href={href}
                className={`shrink-0 rounded-md px-3 py-1.5 text-sm font-medium transition ${
                  active
                    ? "bg-accent-soft text-accent-strong"
                    : "text-zinc-500 hover:bg-zinc-100 hover:text-zinc-900 dark:text-zinc-400 dark:hover:bg-zinc-800/60 dark:hover:text-zinc-100"
                }`}
              >
                {tab.short}
              </Link>
            );
          })}
        </nav>

        <div className="ml-auto flex items-center gap-2">
          <a
            href="https://github.com/l3wi/mc2"
            target="_blank"
            rel="noreferrer"
            className="hidden items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium text-zinc-500 transition hover:text-zinc-900 sm:flex dark:text-zinc-400 dark:hover:text-zinc-100"
          >
            GitHub
          </a>
          <ThemeToggle />
        </div>
      </div>
    </header>
  );
}
