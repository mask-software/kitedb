import { notFound } from "@tanstack/solid-router";
import { findDocBySlug } from "~/lib/docs";

/** "/docs/guides/schema" -> "guides/schema". */
function docSlugFromPath(pathname: string): string {
	const match = pathname.match(/^\/docs\/(.+)$/);
	return match ? match[1] : "";
}

/**
 * Loader shared by the docs splat routes. Throws `notFound()` for slugs that
 * aren't in the docs nav, so the server responds 404 and the route renders its
 * `notFoundComponent` inside the docs layout.
 */
export function loadDocSlug(ctx: { location: { pathname: string } }) {
	const slug = docSlugFromPath(ctx.location.pathname);
	if (!findDocBySlug(slug)) throw notFound();
	return { slug };
}
