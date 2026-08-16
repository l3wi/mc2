import type { Metadata } from "next";
import "./globals.css";
import { TopNav } from "@/components/top-nav";

const themeInit = `(function(){try{var t=localStorage.getItem('theme');var d=t?t==='dark':window.matchMedia('(prefers-color-scheme: dark)').matches;if(d)document.documentElement.classList.add('dark')}catch(e){}})();`;

export const metadata: Metadata = {
  title: {
    default: "MC2 Docs",
    template: "%s — MC2 Docs",
  },
  description:
    "MicroCommandControl (MC2) — Compose-shaped orchestration for microsandbox microVMs.",
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: themeInit }} />
      </head>
      <body className="min-h-screen">
        <TopNav />
        {children}
      </body>
    </html>
  );
}
