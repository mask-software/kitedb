import Link from "next/link";

export default function NotFound() {
  return (
    <div className="min-h-screen flex items-center justify-center bg-[#05070d] text-white p-8">
      <div className="max-w-md text-center">
        <h1 className="text-4xl font-bold text-[#00d4ff] mb-4">Oops!</h1>
        <p className="text-slate-400 mb-6">The page you are looking for does not exist.</p>
        <Link
          href="/"
          className="inline-flex items-center gap-2 px-6 py-3 bg-[#00d4ff] text-black font-semibold rounded-lg hover:bg-[#00d4ff]/90 transition-colors"
        >
          Go Home
        </Link>
      </div>
    </div>
  );
}
