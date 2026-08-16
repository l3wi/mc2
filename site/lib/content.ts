import { compileMDX } from "next-mdx-remote/rsc";
import rehypePrettyCode from "rehype-pretty-code";
import rehypeSlug from "rehype-slug";
import remarkGfm from "remark-gfm";
import fs from "node:fs";
import path from "node:path";
import { mdxComponents } from "@/components/mdx";

export const CONTENT_DIR = path.join(process.cwd(), "content");

export function pageFile(tab: string, pagePath: string): string {
  return path.join(CONTENT_DIR, tab, `${pagePath}.mdx`);
}

export function pageExists(tab: string, pagePath: string): boolean {
  try {
    fs.accessSync(pageFile(tab, pagePath), fs.constants.R_OK);
    return true;
  } catch {
    return false;
  }
}

export type PageMeta = {
  title?: string;
  description?: string;
};

export async function renderPage(tab: string, pagePath: string) {
  const source = fs.readFileSync(pageFile(tab, pagePath), "utf8");

  const { content, frontmatter } = await compileMDX<PageMeta>({
    source,
    components: mdxComponents,
    options: {
      parseFrontmatter: true,
      mdxOptions: {
        remarkPlugins: [remarkGfm],
        rehypePlugins: [
          rehypeSlug,
          [
            rehypePrettyCode,
            {
              themes: { light: "github-light", dark: "github-dark" },
              keepBackground: false,
            },
          ],
        ],
      },
    },
  });

  return { content, meta: frontmatter };
}
