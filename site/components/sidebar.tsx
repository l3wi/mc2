"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useMemo, useState } from "react";
import { ChevronDown } from "lucide-react";
import { tabs, type NavGroup, type NavPage } from "@/lib/nav";
import { GroupIcon } from "./icons";

function Group({
  group,
  tabPath,
  pathname,
}: {
  group: NavGroup;
  tabPath: string;
  pathname: string;
}) {
  const active = group.pages.some((p) => pathname === `${tabPath}/${p.path}`);
  const [open, setOpen] = useState<boolean>(!group.collapsed || active);

  useEffect(() => {
    if (active) setOpen(true);
  }, [active]);

  return (
    <div className="px-2 py-2">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs font-semibold uppercase tracking-wider text-zinc-500 transition hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200"
      >
        <GroupIcon name={group.icon} className="h-3.5 w-3.5 text-accent" />
        <span className="flex-1">{group.title}</span>
        <ChevronDown
          className={`h-3.5 w-3.5 transition-transform ${open ? "" : "-rotate-90"}`}
        />
      </button>

      {open && (
        <ul className="mt-1 space-y-0.5 border-l border-zinc-200 pl-3 dark:border-zinc-800">
          {group.pages
            .filter((p) => !p.hidden)
            .map((page: NavPage) => {
              const href = `${tabPath}/${page.path}`;
              const isActive = pathname === href;
              return (
                <li key={page.path}>
                  <Link
                    href={href}
                    className={`block rounded-md px-2 py-1.5 text-sm leading-snug transition ${
                      isActive
                        ? "bg-accent-soft font-medium text-accent-strong"
                        : "text-zinc-600 hover:bg-zinc-100 hover:text-zinc-900 dark:text-zinc-400 dark:hover:bg-zinc-800/60 dark:hover:text-zinc-100"
                    }`}
                  >
                    {page.title}
                  </Link>
                </li>
              );
            })}
        </ul>
      )}
    </div>
  );
}

export function Sidebar() {
  const pathname = usePathname();
  const tab = useMemo(
    () =>
      tabs.find((t) => pathname === t.path || pathname.startsWith(t.path + "/")) ??
      tabs[0],
    [pathname],
  );

  return (
    <aside className="hidden w-[264px] shrink-0 border-r border-zinc-200 py-6 md:block dark:border-zinc-800">
      <nav className="sticky top-14 max-h-[calc(100vh-3.5rem)] overflow-y-auto pb-10">
        {tab.groups.map((group) => (
          <Group
            key={group.title}
            group={group}
            tabPath={tab.path}
            pathname={pathname}
          />
        ))}
      </nav>
    </aside>
  );
}
