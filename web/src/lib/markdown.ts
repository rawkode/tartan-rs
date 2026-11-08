/**
 * Markdown rendering for issue bodies and long-text properties: GitHub-flavoured Markdown
 * through `marked`, sanitised with DOMPurify because the text comes from other people.
 */

import DOMPurify from "dompurify";
import { Marked } from "marked";

const marked = new Marked({ gfm: true, breaks: false });

/** Renders Markdown to sanitised HTML. Links open in a new tab without an opener. */
export const renderMarkdown = (source: string): string => {
	const html = marked.parse(source, { async: false });
	const clean = DOMPurify.sanitize(html, { USE_PROFILES: { html: true } });
	const template = document.createElement("template");
	template.innerHTML = clean;
	for (const link of template.content.querySelectorAll("a[href]")) {
		link.setAttribute("target", "_blank");
		link.setAttribute("rel", "noopener noreferrer");
	}
	return template.innerHTML;
};
