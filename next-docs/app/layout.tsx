import type { Metadata, Viewport } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "RayDB",
  description:
    "RayDB - High-performance embedded graph database with vector search for Bun and TypeScript",
};

export const viewport: Viewport = {
  themeColor: "#05070d",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en" className="dark">
      <body className="antialiased">
        {children}
      </body>
    </html>
  );
}
