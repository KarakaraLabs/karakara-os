/**
 * What the Notion tool returned during a run, gathered by code so that answering never depends on
 * the model having paged through an archive.
 *
 * Pure functions, no globals: importing this module has no side effects, so it can be tested
 * directly.
 */

export type NotionPageRef = { id: string; title: string; url: string };
type NotionBlock = { block_type?: string; text?: string | null };
type PageContent = { title: string; url: string; text: string };

export type NotionFindings = {
  pages: NotionPageRef[];
  contents: PageContent[];
  /** The tool stopped before the end of a page. */
  incomplete: boolean;
};

/** About 25k tokens. Larger pages are cut, and the answer says so. */
const MAX_CONTENT_CHARS = 100_000;

export const NO_PAGES_MESSAGE =
  "No Notion pages matched this request. If the page exists, it may not be shared with this " +
  "integration: Notion search only returns pages shared with it. Open the page in Notion, then " +
  "use ••• > Connections to add the integration.";

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

export function emptyFindings(): NotionFindings {
  return { pages: [], contents: [], incomplete: false };
}

function pageText(blocks: NotionBlock[]): string {
  return blocks
    .filter((block) => block.block_type !== "missing_hint")
    .map((block) => block.text?.trim() ?? "")
    .filter((text) => text.length > 0)
    .join("\n");
}

/** Record one support/notion output: the pages it names and any page text it carries. */
export function addToolOutput(findings: NotionFindings, output: unknown): void {
  if (!isObject(output)) return;
  const pages = Array.isArray(output.pages) ? (output.pages as NotionPageRef[]) : [];
  for (const page of pages) {
    if (!findings.pages.some((known) => known.id === page.id)) findings.pages.push(page);
  }

  const blocks = Array.isArray(output.blocks) ? (output.blocks as NotionBlock[]) : [];
  const text = pageText(blocks);
  if (!text) return;
  if (output.has_more === true) findings.incomplete = true;

  const title = pages[0]?.title ?? "Untitled page";
  const url = pages[0]?.url ?? "";
  const existing = findings.contents.find((content) => content.url === url);
  if (!existing) {
    findings.contents.push({ title, url, text });
  } else if (!existing.text.includes(text)) {
    existing.text += `\n${text}`;
  }
}

/** Record every tool output from one step-executor run. */
export function addStepResults(findings: NotionFindings, steps: unknown[]): void {
  for (const step of steps) {
    if (isObject(step)) addToolOutput(findings, step.result);
  }
}

/** The findings as `AnswerFromNotion` takes them, within the content size limit. */
export function answerInput(findings: NotionFindings, maxChars: number = MAX_CONTENT_CHARS) {
  let remaining = maxChars;
  let truncated = findings.incomplete;
  const page_contents: PageContent[] = [];
  for (const content of findings.contents) {
    if (remaining <= 0) {
      truncated = true;
      break;
    }
    if (content.text.length > remaining) truncated = true;
    page_contents.push({ ...content, text: content.text.slice(0, remaining) });
    remaining -= content.text.length;
  }
  return {
    found_pages: findings.pages.map(({ title, url }) => ({ title, url })),
    page_contents,
    content_truncated: truncated,
  };
}
