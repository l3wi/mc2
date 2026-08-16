import Link from "next/link";
import { notFound } from "next/navigation";
import { ChevronLeft, ChevronRight, Code2 } from "lucide-react";
import { allPages, orderedPages, tabs, type NavTab } from "@/lib/nav";
import { pageExists, renderPage } from "@/lib/content";
import { Sidebar } from "@/components/sidebar";
import { Toc } from "@/components/toc";

export const dynamic = "force-static";

export function generateStaticParams() {
  return allPages().map(({ tab, page }) => ({
    slug: [tab.path.slice(1), ...page.path.split("/")],
  }));
}

export async function generateMetadata({
  params,
}: {
  params: Promise<{ slug: string[] }>;
}) {
  const slug = (await params).slug;
  const [tabName, ...rest] = slug;
  const pagePath = rest.join("/");
  if (!pageExists(tabName, pagePath)) return {};
  const { meta } = await renderPage(tabName, pagePath);
  return { title: meta.title };
}

function findTab(tabName: string): NavTab | undefined {
  return tabs.find((t) => t.path === `/${tabName}`);
}

export default async function Page({
  params,
}: {
  params: Promise<{ slug: string[] }>;
}) {
  const slug = (await params).slug;
  const [tabName, ...rest] = slug;
  const pagePath = rest.join("/");

  if (!pageExists(tabName, pagePath)) notFound();

  const { content, meta } = await renderPage(tabName, pagePath);
  const tab = findTab(tabName);
  const pages = tab ? orderedPages(tab) : [];
  const currentHref = `/${tabName}/${pagePath}`;
  const index = pages.findIndex((p) => p.href === currentHref);
  const prev = index > 0 ? pages[index - 1] : undefined;
  const next = index >= 0 && index < pages.length - 1 ? pages[index + 1] : undefined;

  return (
    <div className="docs-grid">
      <Sidebar />

      <main className="min-w-0 px-5 py-10 sm:px-8 md:px-10">
        <article className="prose prose-zinc dark:prose-invert">
          <header className="mb-8">
            {tab && (
              <p className="mb-1 text-xs font-semibold uppercase tracking-wider text-accent">
                {tab.name}
              </p>
            )}
            <h1 className="mt-0 text-3xl font-bold tracking-tight text-zinc-900 dark:text-zinc-50">
              {meta.title ?? pagePath}
            </h1>
            {meta.description && (
              <p className="mt-3 text-base leading-relaxed text-zinc-500 dark:text-zinc-400">
                {meta.description}
              </p>
            )}
          </header>

          {content}
        </article>

        <footer className="mt-16 flex flex-col gap-4 border-t border-zinc-200 pt-6 sm:flex-row sm:items-center sm:justify-between dark:border-zinc-800">
          <div className="flex items-center gap-2 text-xs text-zinc-400 dark:text-zinc-500">
            <Code2 className="h-3.5 w-3.5" />
            <span>MicroCommandControl · MIT License</span>
          </div>
          <div className="flex items-center gap-2">
            {prev && (
              <Link
                href={prev.href}
                className="flex items-center gap-1 rounded-md border border-zinc-200 px-3 py-1.5 text-sm text-zinc-600 transition hover:border-accent/50 hover:text-accent dark:border-zinc-800 dark:text-zinc-400"
              >
                <ChevronLeft className="h-4 w-4" />
                {prev.page.title}
              </Link>
            )}
            {next && (
              <Link
                href={next.href}
                className="flex items-center gap-1 rounded-md border border-zinc-200 px-3 py-1.5 text-sm text-zinc-600 transition hover:border-accent/50 hover:text-accent dark:border-zinc-800 dark:text-zinc-400"
              >
                {next.page.title}
                <ChevronRight className="h-4 w-4" />
              </Link>
            )}
          </div>
        </footer>
      </main>

      <Toc />
    </div>
  );
}
