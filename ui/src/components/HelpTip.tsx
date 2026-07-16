import { useId, useState } from 'react';
import { HELP_NOTES, type HelpTopic } from '../helpNotes';

/** In-place micro-help (#164): a small "?" affordance that answers the
 * load-bearing jargon where it appears. Keyboard reachable (one Tab, one
 * Enter), hover-friendly, Esc dismisses; the note text comes from the
 * single-sourced `HELP_NOTES` so surfaces can never drift apart. */
export function HelpTip({
  topic,
  align = 'start',
}: {
  topic: HelpTopic;
  /** Popover alignment: 'end' hangs the note leftward from the trigger —
   * use it when the trigger sits near a clipping right edge. */
  align?: 'start' | 'end';
}) {
  const { term, note, learnMoreUrl } = HELP_NOTES[topic];
  const [open, setOpen] = useState(false);
  const id = useId();
  return (
    <span
      className="help-tip"
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      onBlur={(event) => {
        // Close only when focus leaves the whole affordance — tabbing from
        // the trigger to the Learn-more link must not dismiss the note.
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
          setOpen(false);
        }
      }}
      onKeyDown={(event) => {
        if (event.key === 'Escape' && open) {
          event.stopPropagation();
          setOpen(false);
        }
      }}
    >
      <button
        type="button"
        className="help-tip-trigger"
        aria-label={`What is ${term.toLowerCase()}?`}
        aria-expanded={open}
        aria-describedby={open ? id : undefined}
        onClick={() => setOpen((value) => !value)}
      >
        ?
      </button>
      {open && (
        <span role="note" id={id} className={`help-tip-note align-${align}`}>
          <strong>{term}.</strong> {note}{' '}
          <a href={learnMoreUrl} target="_blank" rel="noreferrer">
            Learn more
          </a>
        </span>
      )}
    </span>
  );
}
