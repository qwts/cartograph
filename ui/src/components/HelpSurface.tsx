import type { ReactNode } from 'react';
import { HELP_HOME } from '../helpNotes';
import { HELP_TOPICS } from '../helpTopics';

export interface HelpSurfaceProps {
  /** Active topic slug; unknown slugs fall back to the first topic. */
  topic: string;
  onTopicChange: (slug: string) => void;
}

/** Inline emphasis for our own controlled help markdown: `code` and
 * **bold**. No HTML passes through — everything renders as elements. */
function inline(text: string, keyPrefix: string): ReactNode[] {
  return text.split(/(\*\*[^*]+\*\*|`[^`]+`)/u).map((part, index) => {
    const key = `${keyPrefix}:${index}`;
    if (part.startsWith('**') && part.endsWith('**')) {
      return <strong key={key}>{part.slice(2, -2)}</strong>;
    }
    if (part.startsWith('`') && part.endsWith('`')) {
      return <code key={key}>{part.slice(1, -1)}</code>;
    }
    return <span key={key}>{part}</span>;
  });
}

/** Minimal deterministic renderer for the bundled help markdown (#154):
 * headings, paragraphs, and (nested-flat) bullet/numbered lists. The
 * content is authored in-repo — this is a formatter, not a sanitizer. */
export function renderHelpMarkdown(markdown: string): ReactNode[] {
  const blocks: ReactNode[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  let paragraph: string[] = [];
  const flushList = () => {
    if (!list) return;
    const items = list.items.map((item, index) => (
      <li key={`li:${blocks.length}:${index}`}>{inline(item, `li:${blocks.length}:${index}`)}</li>
    ));
    blocks.push(
      list.ordered ? (
        <ol key={`list:${blocks.length}`}>{items}</ol>
      ) : (
        <ul key={`list:${blocks.length}`}>{items}</ul>
      ),
    );
    list = null;
  };
  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    const text = paragraph.join(' ');
    blocks.push(<p key={`p:${blocks.length}`}>{inline(text, `p:${blocks.length}`)}</p>);
    paragraph = [];
  };
  for (const line of markdown.split('\n')) {
    const heading = line.match(/^(#{1,3}) (.+)$/u);
    const bullet = line.match(/^- (.+)$/u);
    const numbered = line.match(/^\d+\. (.+)$/u);
    const continuation = line.match(/^ {2,}(.+)$/u);
    if (heading) {
      flushList();
      flushParagraph();
      const level = heading[1].length;
      const key = `h:${blocks.length}`;
      blocks.push(
        level === 1 ? (
          <h2 key={key}>{inline(heading[2], key)}</h2>
        ) : (
          <h3 key={key}>{inline(heading[2], key)}</h3>
        ),
      );
    } else if (bullet || numbered) {
      flushParagraph();
      const ordered = Boolean(numbered);
      const item = (bullet ?? numbered)![1];
      if (!list || list.ordered !== ordered) {
        flushList();
        list = { ordered, items: [] };
      }
      list.items.push(item);
    } else if (continuation && list) {
      list.items[list.items.length - 1] += ` ${continuation[1]}`;
    } else if (line.trim() === '') {
      flushList();
      flushParagraph();
    } else {
      flushList();
      paragraph.push(line.trim());
    }
  }
  flushList();
  flushParagraph();
  return blocks;
}

/** In-app Help (#154/#155): sidebar TOC plus the bundled topic content —
 * fully offline, keyboard navigable, single-sourced with the wiki. */
export function HelpSurface({ topic, onTopicChange }: HelpSurfaceProps) {
  const active = HELP_TOPICS.find((entry) => entry.slug === topic) ?? HELP_TOPICS[0];
  return (
    <section className="help-surface" aria-label="Help">
      <nav className="help-toc" aria-label="Help topics">
        <h2>Help</h2>
        <ul>
          {HELP_TOPICS.map((entry) => (
            <li key={entry.slug}>
              <button
                type="button"
                aria-current={entry.slug === active.slug ? 'page' : undefined}
                className={entry.slug === active.slug ? 'active' : ''}
                onClick={() => onTopicChange(entry.slug)}
              >
                {entry.title}
              </button>
            </li>
          ))}
        </ul>
        <a href={HELP_HOME} target="_blank" rel="noreferrer">
          User guide (wiki)
        </a>
      </nav>
      <article className="help-content" aria-live="polite">
        {renderHelpMarkdown(active.markdown)}
      </article>
    </section>
  );
}
