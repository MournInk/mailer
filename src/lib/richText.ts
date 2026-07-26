/**
 * Markdown + LaTeX rendering for assistant answers.
 *
 * Models answer in Markdown whether or not you ask them to, so showing the reply
 * as plain text means showing the user `- **验证码邮件**：…` and a pipe-delimited
 * table. They also reach for LaTeX the moment a number needs structure.
 *
 * The pipeline is ordered so that nothing trusted and nothing generated ever
 * gets confused:
 *
 *   1. lift the math out, leaving opaque tokens behind — before Markdown runs,
 *      because `$x_1$` and `_1_` fight over the underscore, and after code has
 *      been located, because a `$` inside code is a dollar sign;
 *   2. Markdown → HTML;
 *   3. sanitize that HTML — it came from a model, which was in turn fed
 *      attacker-controlled mail, so it is untrusted all the way down;
 *   4. put the KaTeX output back. It is generated here from the extracted
 *      source, so it is ours; running it through the sanitizer would only strip
 *      the inline geometry KaTeX needs to place glyphs.
 */

import DOMPurify from "dompurify";
import katex from "katex";
import { marked } from "marked";
import "katex/dist/katex.min.css";

/** One lifted formula. */
interface Formula {
  tex: string;
  display: boolean;
}

/**
 * Placeholder left where a formula was.
 *
 * Letters and digits only: anything with punctuation risks being escaped or
 * linkified by the Markdown parser, and the token has to survive unchanged to be
 * found again. The leading `x` keeps it from ever parsing as a bare number.
 */
const token = (i: number) => `xKaTeXFormula${i}Endx`;

/** Inline and display delimiters, longest first so `$$` wins over `$`. */
const DELIMS: Array<{ open: string; close: string; display: boolean }> = [
  { open: "$$", close: "$$", display: true },
  { open: "\\[", close: "\\]", display: true },
  { open: "\\(", close: "\\)", display: false },
  { open: "$", close: "$", display: false },
];

/**
 * Replace every formula with a token, returning the rewritten source.
 *
 * Walks the string once, skipping fenced blocks and inline code so their
 * contents stay literal. A `$` with no closing partner is left alone — prose
 * about money ("成本 $42") must not be swallowed as an unterminated formula.
 */
function liftMath(src: string, out: Formula[]): string {
  let text = "";
  let i = 0;

  while (i < src.length) {
    // Fenced code: copy through to the closing fence (or the end).
    const fence = /^(`{3,}|~{3,})/.exec(src.slice(i));
    if (fence && (i === 0 || src[i - 1] === "\n")) {
      const marker = fence[1];
      const end = src.indexOf(`\n${marker}`, i + marker.length);
      const stop = end === -1 ? src.length : end + marker.length + 1;
      text += src.slice(i, stop);
      i = stop;
      continue;
    }

    // Inline code: same idea, bounded by a matching run of backticks.
    if (src[i] === "`") {
      const run = /^`+/.exec(src.slice(i))![0];
      const end = src.indexOf(run, i + run.length);
      const stop = end === -1 ? i + run.length : end + run.length;
      text += src.slice(i, stop);
      i = stop;
      continue;
    }

    // An escaped delimiter is a literal one.
    if (src[i] === "\\" && (src[i + 1] === "$" || src[i + 1] === "\\")) {
      text += src.slice(i, i + 2);
      i += 2;
      continue;
    }

    const delim = DELIMS.find((d) => src.startsWith(d.open, i));
    if (delim) {
      const from = i + delim.open.length;
      const end = src.indexOf(delim.close, from);
      const tex = end === -1 ? "" : src.slice(from, end).trim();
      // Unterminated, or empty (`$$`): not a formula, just characters.
      if (end !== -1 && tex) {
        text += token(out.length);
        out.push({ tex, display: delim.display });
        i = end + delim.close.length;
        continue;
      }
    }

    text += src[i];
    i += 1;
  }

  return text;
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** Render one formula, or fall back to showing its source. */
function renderFormula(f: Formula): string {
  try {
    return katex.renderToString(f.tex, {
      displayMode: f.display,
      // A malformed formula becomes visible source rather than an exception that
      // would cost the user the whole answer.
      throwOnError: false,
      output: "html",
      strict: false,
      // `\href`, `\includegraphics` and friends stay off: the TeX came from a
      // model that has been reading untrusted mail.
      trust: false,
    });
  } catch {
    const tag = f.display ? "div" : "span";
    return `<${tag} class="rt-math-raw">${escapeHtml(f.tex)}</${tag}>`;
  }
}

/**
 * Tag/attribute policy for model-authored Markdown.
 *
 * Scripting, embedded documents and forms are gone. Links survive — an answer
 * that cites a URL out of a mail is useful — but they are neutered by the hook
 * below. `style` is dropped because nothing in an answer needs to paint itself.
 */
const SANITIZE = {
  FORBID_TAGS: [
    "script", "iframe", "object", "embed", "form", "style", "link", "base",
    "meta", "input", "button", "select", "textarea", "noscript",
  ],
  FORBID_ATTR: ["srcset", "background", "ping", "formaction", "style"],
  // No remote fetch of any kind from an answer: an <img> the model was talked
  // into emitting would be a tracking pixel with the user's IP on it.
  ALLOWED_URI_REGEXP: /^(?:https?|mailto):/i,
};

/**
 * Markdown (+ LaTeX) → sanitized HTML, ready for `dangerouslySetInnerHTML`.
 *
 * The name is the warning: the input is untrusted, and this is the only function
 * allowed to turn it into markup.
 */
export function renderRichText(source: string): string {
  const formulas: Formula[] = [];
  const withTokens = liftMath(source, formulas);
  const html = marked.parse(withTokens, { async: false, breaks: true, gfm: true });

  // Scoped, not global: `MessageView` sanitizes mail bodies with its own hook
  // and depends on <img> surviving so it can gate remote images. A hook left
  // installed here would silently strip every picture out of the user's mail.
  const hook = (node: Element) => {
    if (node.tagName === "A") {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer nofollow");
    }
    // A Markdown image in a chat answer is always remote and never wanted.
    if (node.tagName === "IMG") node.remove();
  };

  DOMPurify.addHook("afterSanitizeAttributes", hook);
  let clean: string;
  try {
    clean = DOMPurify.sanitize(html, SANITIZE);
  } finally {
    DOMPurify.removeHook("afterSanitizeAttributes", hook);
  }

  for (let i = 0; i < formulas.length; i += 1) {
    // Tokens are unique and alphanumeric, so split/join is exact and cannot be
    // defeated by a formula that happens to render something token-shaped.
    clean = clean.split(token(i)).join(renderFormula(formulas[i]));
  }
  return clean;
}
