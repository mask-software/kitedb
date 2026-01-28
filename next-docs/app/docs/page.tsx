import Link from "next/link";
import {
  ArrowRight,
  BookOpen,
  Code,
  Database,
  GitBranch,
  Rocket,
  Zap,
} from "lucide-react";
import { Card, CardGrid } from "../../components/Card";
import { docsStructure } from "../../lib/docs";

export default function DocsIndexPage() {
  return (
    <div className="max-w-4xl mx-auto px-6 py-12">
      <div className="mb-12">
        <h1 className="text-4xl font-extrabold text-slate-900 dark:text-white tracking-tight text-balance">
          RayDB Documentation
        </h1>
        <p className="mt-4 text-lg text-slate-600 dark:text-slate-400 text-pretty">
          Learn how to build high-performance graph databases with vector search using RayDB. From quick starts to deep dives into the architecture.
        </p>
      </div>

      <section className="mb-16" aria-labelledby="quickstart-heading">
        <h2 id="quickstart-heading" className="text-xl font-bold text-slate-900 dark:text-white mb-6">
          Get Started
        </h2>
        <div className="grid sm:grid-cols-2 gap-4">
          <Link
            href="/docs/getting-started/installation"
            className="group flex items-start gap-4 p-6 rounded-2xl bg-gradient-to-br from-cyan-500/5 via-transparent to-violet-500/5 border border-cyan-500/20 hover:border-cyan-500/40 hover:shadow-lg hover:shadow-cyan-500/5 transition-all duration-200"
          >
            <div className="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-gradient-to-br from-cyan-500 to-cyan-600 text-white shadow-lg shadow-cyan-500/30">
              <Rocket size={24} aria-hidden="true" />
            </div>
            <div className="min-w-0">
              <h3 className="font-semibold text-slate-900 dark:text-white group-hover:text-cyan-600 dark:group-hover:text-cyan-400 transition-colors duration-150">
                Installation
              </h3>
              <p className="mt-1 text-sm text-slate-600 dark:text-slate-400">
                Set up RayDB in your project in under 2 minutes.
              </p>
            </div>
            <ArrowRight
              size={20}
              className="flex-shrink-0 text-slate-400 group-hover:text-cyan-500 group-hover:translate-x-1 transition-all duration-150 mt-1"
              aria-hidden="true"
            />
          </Link>

          <Link
            href="/docs/getting-started/quick-start"
            className="group flex items-start gap-4 p-6 rounded-2xl bg-gradient-to-br from-violet-500/5 via-transparent to-cyan-500/5 border border-violet-500/20 hover:border-violet-500/40 hover:shadow-lg hover:shadow-violet-500/5 transition-all duration-200"
          >
            <div className="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-gradient-to-br from-violet-500 to-violet-600 text-white shadow-lg shadow-violet-500/30">
              <Code size={24} aria-hidden="true" />
            </div>
            <div className="min-w-0">
              <h3 className="font-semibold text-slate-900 dark:text-white group-hover:text-violet-600 dark:group-hover:text-violet-400 transition-colors duration-150">
                Quick Start
              </h3>
              <p className="mt-1 text-sm text-slate-600 dark:text-slate-400">
                Build your first graph database in 5 minutes.
              </p>
            </div>
            <ArrowRight
              size={20}
              className="flex-shrink-0 text-slate-400 group-hover:text-violet-500 group-hover:translate-x-1 transition-all duration-150 mt-1"
              aria-hidden="true"
            />
          </Link>
        </div>
      </section>

      {docsStructure.map((section) => (
        <section
          key={section.label}
          className="mb-12"
          aria-labelledby={`section-${section.label.toLowerCase().replace(/\s+/g, "-")}`}
        >
          <h2
            id={`section-${section.label.toLowerCase().replace(/\s+/g, "-")}`}
            className="text-xl font-bold text-slate-900 dark:text-white mb-6"
          >
            {section.label}
          </h2>
          <CardGrid columns={2}>
            {section.items.map((item) => (
              <Card
                key={item.slug}
                title={item.title}
                description={item.description}
                href={`/docs/${item.slug}`}
                icon={
                  item.slug.includes("installation") ? (
                    <Rocket size={20} aria-hidden="true" />
                  ) : item.slug.includes("quick-start") ? (
                    <Zap size={20} aria-hidden="true" />
                  ) : item.slug.includes("schema") ? (
                    <Database size={20} aria-hidden="true" />
                  ) : item.slug.includes("traversal") ? (
                    <GitBranch size={20} aria-hidden="true" />
                  ) : (
                    <BookOpen size={20} aria-hidden="true" />
                  )
                }
              />
            ))}
          </CardGrid>
        </section>
      ))}
    </div>
  );
}
