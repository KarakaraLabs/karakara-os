export type NotionPageRef = {
  id: string;
  title: string;
  url: string;
  last_edited_time?: string | null;
};

type NotionBlock = { block_type?: string; text?: string | null };
type PageContent = { id: string; title: string; url: string; text: string };

export type NotionFindings = {
  contents: PageContent[];
  /** The Notion API stopped before the end of a page body. */
  incomplete: boolean;
};

/** About 25k tokens. Larger results are cut before the final answer call. */
const MAX_CONTENT_CHARS = 100_000;

export const NO_PAGES_MESSAGE =
  "No Notion pages matched this request. If the page exists, it may not be shared with this " +
  "integration: Notion search only returns pages shared with it. Open the page in Notion, then " +
  "use ••• > Connections to add the integration.";

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

export function emptyFindings(): NotionFindings {
  return { contents: [], incomplete: false };
}

function pageText(blocks: NotionBlock[]): string {
  return blocks
    .filter((block) => block.block_type !== "missing_hint")
    .map((block) => block.text?.trim() ?? "")
    .filter((text) => text.length > 0)
    .join("\n");
}

/** Record page text from one direct support/notion call. */
export function addToolOutput(
  findings: NotionFindings,
  output: unknown,
  requestedPage?: NotionPageRef,
): void {
  if (!isObject(output)) return;
  const pages = Array.isArray(output.pages)
    ? (output.pages as NotionPageRef[])
    : [];
  const blocks = Array.isArray(output.blocks)
    ? (output.blocks as NotionBlock[])
    : [];
  const text = pageText(blocks);
  if (!text) return;
  if (output.has_more === true) findings.incomplete = true;

  const page = pages[0] ?? requestedPage;
  const id = page?.id ?? "";
  const title = page?.title ?? "Untitled page";
  const url = page?.url ?? "";
  const existing = findings.contents.find((content) =>
    id ? content.id === id : content.url === url,
  );
  if (!existing) {
    findings.contents.push({ id, title, url, text });
  } else if (!existing.text.includes(text)) {
    existing.text += `\n${text}`;
  }
}

type AnswerInputOptions = {
  selectedPages: NotionPageRef[];
  matchedPageCount?: number;
  resultListTruncated?: boolean;
  searchTruncated?: boolean;
  maxChars?: number;
};

function allocateContentChars(
  contents: PageContent[],
  maxChars: number,
): number[] {
  const allocations = contents.map(() => 0);
  let remaining = Math.max(0, maxChars);
  let pending = contents.map((_, index) => index);

  while (remaining > 0 && pending.length > 0) {
    const fairShare = Math.floor(remaining / pending.length);
    if (fairShare === 0) {
      for (const index of pending.slice(0, remaining)) allocations[index]++;
      break;
    }

    const short = pending.filter(
      (index) => contents[index]!.text.length <= fairShare,
    );
    if (short.length === 0) {
      for (const index of pending) allocations[index] = fairShare;
      remaining -= fairShare * pending.length;
      for (const index of pending.slice(0, remaining)) allocations[index]++;
      break;
    }

    for (const index of short) {
      allocations[index] = contents[index]!.text.length;
      remaining -= allocations[index]!;
    }
    const shortSet = new Set(short);
    pending = pending.filter((index) => !shortSet.has(index));
  }

  return allocations;
}

/** Shape the selected evidence for AnswerFromNotion and enforce its content limit. */
export function answerInput(
  findings: NotionFindings,
  options: AnswerInputOptions,
) {
  const selectedPages = options.selectedPages;
  const matchedPageCount = options.matchedPageCount ?? selectedPages.length;
  const resultListTruncated = options.resultListTruncated ?? false;
  const searchTruncated = options.searchTruncated ?? false;
  const maxChars = options.maxChars ?? MAX_CONTENT_CHARS;
  const contentsById = new Map(
    findings.contents.map((content) => [content.id, content]),
  );
  const selectedContents = selectedPages
    .map((page) => contentsById.get(page.id))
    .filter((content): content is PageContent => content !== undefined);
  const allocations = allocateContentChars(selectedContents, maxChars);
  let truncated = findings.incomplete;
  const page_contents = selectedContents.map((content, index) => {
    const text = content.text.slice(0, allocations[index]);
    if (text.length < content.text.length) truncated = true;
    return { ...content, text };
  });

  return {
    matched_page_count: matchedPageCount,
    found_pages: selectedPages.map(({ id, title, url, last_edited_time }) => ({
      id,
      title,
      url,
      last_edited_time: last_edited_time ?? null,
    })),
    page_contents,
    content_truncated: truncated,
    result_list_truncated: resultListTruncated,
    search_truncated: searchTruncated,
  };
}
