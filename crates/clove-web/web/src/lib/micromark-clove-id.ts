// Custom micromark extension: autolink clove ids in normal text.
//
// A clove id is one of:
//   - prefixed:  #<prefix>-<8 Crockford base32>   e.g. #proj-7af3q2k9
//                prefix = [a-z][a-z0-9]*  (lowercase, starts with a letter)
//   - bare:      #<8 Crockford base32>             e.g. #7AF3Q2K9
//
// The 8-char id body is Crockford base32, which EXCLUDES the letters
// I, L, O, U (to avoid look-alikes). So the accepted body alphabet is
//   0-9 A-H J K M N P-T V-Z   (and their lowercase equivalents).
//
// This is a real micromark *syntax* extension (a `text` construct hooked on
// the `#` character code) plus a matching *HTML compiler* extension. It does
// NOT use a regular expression or a hand-rolled scanner over rendered HTML:
// every character is classified by its code point through micromark's own
// `micromark-util-character` helpers and explicit code-point comparisons.
//
// Because it is registered as a `text` construct it only runs where inline
// text is being tokenized — micromark's construct model means it never fires
// inside code spans, fenced/indented code blocks, or the target/label of an
// existing link or autolink. (Covered by tests.)

import { codes } from 'micromark-util-symbol';
import { asciiAlpha, asciiDigit } from 'micromark-util-character';
import { encode } from 'micromark-util-encode';
import type {
  Code,
  CompileContext,
  Construct,
  Extension,
  HtmlExtension,
  State,
  Token,
  Tokenizer
} from 'micromark-util-types';

// Register our token type with micromark via declaration merging, the same way
// the official extensions do. This makes `'cloveId'` a known token name for
// `effects.enter/exit` and for the HTML compiler's `exit` handler map.
declare module 'micromark-util-types' {
  interface TokenTypeMap {
    cloveId: 'cloveId';
  }
}

// Number of base32 characters in the id body.
const ID_BODY_LENGTH = 8;

// Crockford base32: digits 0-9 plus A-Z minus the four ambiguous letters
// I, L, O, U. We classify by code point, comparing against ASCII ranges and
// rejecting the excluded letters explicitly — no regex, no lookup table walk.
function isCrockfordBase32(code: Code): boolean {
  if (code === null) return false;
  if (asciiDigit(code)) return true; // 0-9
  // Normalise letters to uppercase code point for a single range check.
  // 'a'(97)-'z'(122) -> 'A'(65)-'Z'(90)
  let c = code;
  if (c >= codes.lowercaseA && c <= codes.lowercaseZ) {
    c = c - (codes.lowercaseA - codes.uppercaseA);
  }
  if (c < codes.uppercaseA || c > codes.uppercaseZ) return false;
  // Reject the Crockford-excluded letters I, L, O, U.
  if (
    c === codes.uppercaseI ||
    c === codes.uppercaseL ||
    c === codes.uppercaseO ||
    c === codes.uppercaseU
  ) {
    return false;
  }
  return true;
}

// Prefix characters: lowercase ASCII letters and digits (the first must be a
// letter, enforced separately). Classify by code point.
function isLowerAlnum(code: Code): boolean {
  if (code === null) return false;
  if (asciiDigit(code)) return true;
  return code >= codes.lowercaseA && code <= codes.lowercaseZ;
}

// A character that, if it FOLLOWS a complete id, means the id ran into a longer
// token and must not be linked (right word boundary). Any base32 body char,
// '-', or an alphanumeric would extend/blur the id.
function isIdContinuation(code: Code): boolean {
  if (code === null) return false;
  if (code === codes.dash) return true;
  // Letters or digits would make the "8 chars" claim ambiguous.
  return asciiAlpha(code) || asciiDigit(code);
}

const cloveIdTokenize: Tokenizer = function (effects, ok, nok) {
  const self = this;

  return start;

  // Left word boundary: a `#` only begins an id at the start of the text, or
  // after whitespace/line ending, or after punctuation — never mid-word
  // (e.g. `abc#deadbeef`). We inspect the previously emitted character code.
  function start(code: Code): State | undefined {
    // `code` here is the `#` (codes.numberSign === 35); the construct is only
    // invoked for that code, but assert defensively.
    if (code !== codes.numberSign) return nok(code);

    const previous = self.previous;
    // Disallow if the preceding char was alphanumeric (would be mid-word).
    if (previous !== null && (asciiAlpha(previous) || asciiDigit(previous))) {
      return nok(code);
    }

    effects.enter('cloveId');
    effects.consume(code); // consume '#'
    return afterHash;
  }

  // After '#': either a prefix letter (prefixed form) or directly a base32
  // body char (bare form). We do not know yet which form it is; we try to
  // read an optional `prefix-` then a mandatory 8-char body.
  function afterHash(code: Code): State | undefined {
    // Bare form starts immediately with a base32 char; prefixed form starts
    // with a lowercase letter that is part of the prefix. A lowercase letter
    // is valid in BOTH, so we tentatively read prefix chars and decide at the
    // '-' whether it was a prefix. To keep the construct deterministic we read
    // the first segment of [a-z0-9]* characters; if it is followed by '-' it
    // was a prefix, otherwise it must have been the start of the 8-char body.
    if (code !== null && code >= codes.lowercaseA && code <= codes.lowercaseZ) {
      // A lowercase letter must open the prefix (prefixes start with a letter).
      return prefixOrBody(code, 0, 0);
    }
    if (isCrockfordBase32(code)) {
      // Cannot be a prefix (prefix must start with a letter) -> bare body.
      return body(code, 0);
    }
    return nok(code);
  }

  // Read a run that is simultaneously a candidate prefix and a candidate body.
  // `letters` counts chars consumed in this run; `bodyMatched` tracks whether
  // every char so far is also a valid base32 body char (so we can fall back to
  // the bare form if no '-' appears).
  //
  // We do not actually need to count base32 validity for the prefix path; we
  // re-decide at the boundary:
  //   - if we hit '-', the run was a prefix -> read the 8-char body next.
  //   - if we hit a non-prefix char (or boundary), the run must itself be the
  //     8-char body -> validate length and that all chars were base32.
  function prefixOrBody(code: Code, runLen: number, badForBody: number): State | undefined {
    if (isLowerAlnum(code)) {
      // Track whether this char would be invalid as a base32 body char (e.g.
      // lowercase i/l/o/u, which are excluded from Crockford).
      const bad = isCrockfordBase32(code) ? 0 : 1;
      effects.consume(code);
      return (next: Code) => prefixOrBody(next, runLen + 1, badForBody + bad);
    }

    if (code === codes.dash) {
      // The run was a prefix (it started with a letter, enforced in afterHash).
      // A prefix must be non-empty.
      if (runLen < 1) return nok(code);
      effects.consume(code); // consume '-'
      return (next: Code) => body(next, 0);
    }

    // No '-': the run itself must be the bare 8-char base32 body.
    if (runLen === ID_BODY_LENGTH && badForBody === 0) {
      // Right boundary: the next char must not continue the id.
      if (isIdContinuation(code)) return nok(code);
      return done(code);
    }
    return nok(code);
  }

  // Read exactly ID_BODY_LENGTH base32 chars as the id body (prefixed form, or
  // the bare form reached directly).
  function body(code: Code, count: number): State | undefined {
    if (count < ID_BODY_LENGTH) {
      if (!isCrockfordBase32(code)) return nok(code);
      effects.consume(code);
      return (next: Code) => body(next, count + 1);
    }
    // count === ID_BODY_LENGTH: enforce right boundary.
    if (isIdContinuation(code)) return nok(code);
    return done(code);
  }

  function done(code: Code): State | undefined {
    effects.exit('cloveId');
    return ok(code);
  }
};

const cloveIdConstruct: Construct = {
  name: 'cloveId',
  tokenize: cloveIdTokenize,
  // Left word boundary: micromark only attempts this construct when `previous`
  // returns true. We allow it at the start of text (previous === null) or when
  // the preceding character is NOT alphanumeric, so `abc#deadbeef` won't fire
  // mid-word. Classification is by code point (no regex).
  previous(code: Code) {
    return code === null || !(asciiAlpha(code) || asciiDigit(code));
  }
};

/**
 * micromark syntax extension: enables clove-id autolinking in inline text.
 * Hook on the `#` character code (35) in the `text` context.
 */
export function cloveId(): Extension {
  return {
    text: {
      [codes.numberSign]: cloveIdConstruct
    }
  };
}

/** Options for [`cloveIdHtml`]. */
export interface CloveIdHtmlOptions {
  /** The app base path prepended to hrefs (SvelteKit's `$app/paths` base). */
  base?: string;
  /**
   * The repository id prefix (from `/meta`), used to resolve the bare
   * `#7AF3Q2K9` form to a full `proj-7AF3Q2K9` id. Without it a bare match is
   * rendered as plain text — a link to a prefixless id can only 404.
   */
  idPrefix?: string;
}

/**
 * micromark HTML compiler extension: renders a `cloveId` token as
 * `<a href="<base>/items/<canonical-id>">#<matched></a>`.
 *
 * The tokenizer matches ids case-insensitively and with or without a prefix,
 * but the API's id grammar is strict (`^[a-z][a-z0-9]{0,7}-[0-9A-Z]{8}$`), so
 * the href canonicalizes what was matched: the suffix is upper-cased and a
 * bare suffix gets `idPrefix` prepended. The visible text stays as written.
 * Both the href and the visible text are escaped with micromark's `encode`
 * utility (no manual escaping / no regex).
 */
export function cloveIdHtml(options: CloveIdHtmlOptions = {}): HtmlExtension {
  const base = options.base ?? '';
  const idPrefix = options.idPrefix ?? '';
  return {
    exit: {
      cloveId(this: CompileContext, token: Token) {
        // The token spans the whole match including the leading `#`; read it
        // straight from the source via micromark's slice helper.
        const matched = this.sliceSerialize(token); // e.g. "#proj-7af3q2k9"
        const id = matched.slice(1); // strip leading '#'
        const dash = id.lastIndexOf('-');
        const prefix = (dash === -1 ? idPrefix : id.slice(0, dash)).toLowerCase();
        const body = (dash === -1 ? id : id.slice(dash + 1)).toUpperCase();
        if (!prefix) {
          // Bare id with no repo prefix known: plain text beats a dead link.
          this.raw(encode(matched));
          return;
        }
        const href = encode(`${base}/items/${prefix}-${body}`);
        this.tag('<a href="' + href + '">');
        this.raw(encode(matched));
        this.tag('</a>');
      }
    }
  };
}
