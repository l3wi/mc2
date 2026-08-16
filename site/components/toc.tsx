"use client";

import { useEffect, useState } from "react";

type Heading = { id: string; text: string; level: number };

export function Toc() {
  const [headings, setHeadings] = useState<Heading[]>([]);
  const [activeId, setActiveId] = useState<string>("");

  useEffect(() => {
    const article = document.querySelector("article");
    if (!article) return;

    const nodes = Array.from(
      article.querySelectorAll<HTMLElement>("h2, h3"),
    ).filter((h) => h.id);
    setHeadings(
      nodes.map((h) => ({
        id: h.id,
        text: h.textContent ?? "",
        level: Number(h.tagName[1]),
      })),
    );

    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            setActiveId((entry.target as HTMLElement).id);
          }
        }
      },
      { rootMargin: "-80px 0px -70% 0px" },
    );
    for (const h of nodes) observer.observe(h);
    return () => observer.disconnect();
  }, []);

  if (headings.length === 0) return null;

  return (
    <aside className="hidden w-[224px] shrink-0 py-10 lg:block">
      <nav className="sticky top-14 max-h-[calc(100vh-3.5rem)] overflow-y-auto">
        <p className="mb-3 px-3 text-xs font-semibold uppercase tracking-wider text-zinc-400 dark:text-zinc-500">
          On this page
        </p>
        <ul className="space-y-1 border-l border-zinc-200 dark:border-zinc-800">
          {headings.map((h) => (
            <li key={h.id}>
              <a
                href={`#${h.id}`}
                className={`block truncate border-l-2 px-3 py-1 text-sm transition ${
                  h.level === 3 ? "pl-7" : "pl-4"
                } ${
                  activeId === h.id
                    ? "border-accent font-medium text-accent-strong"
                    : "border-transparent text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200"
                }`}
              >
                {h.text}
              </a>
            </li>
          ))}
        </ul>
      </nav>
    </aside>
  );
}
