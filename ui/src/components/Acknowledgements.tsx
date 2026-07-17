import { THIRD_PARTY_NOTICES } from '../generated/thirdPartyNotices';

/** In-app "Open-source licenses" view (#222): renders the generated
 *  third-party attribution + license texts. Lazy-loaded from Settings so the
 *  (large) notices string is code-split out of the main bundle. */
export default function Acknowledgements() {
  return (
    <div className="acknowledgements" aria-label="Open-source licenses">
      <p className="muted">
        Cartograph is source-available under the PolyForm Noncommercial License 1.0.0. It
        incorporates the third-party open-source software listed below, each under its own
        license.
      </p>
      <pre className="acknowledgements-text" tabIndex={0}>
        {THIRD_PARTY_NOTICES}
      </pre>
    </div>
  );
}
