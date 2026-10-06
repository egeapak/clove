// A themed hover/focus tooltip. It floats on <body> with fixed positioning so
// list and timeline containers that clip overflow cannot cut it off; one
// element is shared by every trigger.
//
// Content is plain text or a list of segments, each rendered as a text node or
// a styled <span> via `textContent` — never markup — so item titles and other
// user data cannot inject HTML.

/** A styled run of tooltip text; `color` is a CSS colour, e.g. `var(--green)`. */
export interface TooltipSpan {
  text: string;
  color?: string;
  bold?: boolean;
}

export type TooltipContent = string | Array<string | TooltipSpan>;

export interface TooltipOptions {
  content: TooltipContent;
  /** Show only while the anchor's own text is clipped (an ellipsis or a line clamp). */
  whenTruncated?: boolean;
}

export type TooltipParam = TooltipContent | TooltipOptions;

let tip: HTMLDivElement | null = null;
const anchors = new WeakMap<Element, TooltipOptions>();

function element(): HTMLDivElement {
  if (!tip?.isConnected) {
    tip = document.createElement('div');
    tip.className = 'clove-tooltip';
    tip.setAttribute('role', 'tooltip');
    tip.hidden = true;
    document.body.appendChild(tip);
  }
  return tip;
}

function normalize(param: TooltipParam): TooltipOptions {
  return typeof param === 'string' || Array.isArray(param) ? { content: param } : param;
}

function isEmpty(content: TooltipContent): boolean {
  if (typeof content === 'string') return content === '';
  return content.every((part) => (typeof part === 'string' ? part : part.text) === '');
}

function truncated(node: HTMLElement): boolean {
  return node.scrollWidth > node.clientWidth || node.scrollHeight > node.clientHeight;
}

function fill(el: HTMLElement, content: TooltipContent) {
  el.replaceChildren();
  for (const part of typeof content === 'string' ? [content] : content) {
    if (typeof part === 'string') {
      el.append(part);
      continue;
    }
    const span = document.createElement('span');
    span.textContent = part.text;
    if (part.color) span.style.color = part.color;
    if (part.bold) span.style.fontWeight = '700';
    el.append(span);
  }
}

function show(anchor: HTMLElement, options: TooltipOptions) {
  if (isEmpty(options.content) || (options.whenTruncated && !truncated(anchor))) {
    hide();
    return;
  }
  const el = element();
  fill(el, options.content);
  el.hidden = false;
  const a = anchor.getBoundingClientRect();
  const t = el.getBoundingClientRect();
  const margin = 6;
  let top = a.top - t.height - margin;
  if (top < margin) top = a.bottom + margin;
  const left = Math.min(
    Math.max(margin, a.left + a.width / 2 - t.width / 2),
    window.innerWidth - t.width - margin
  );
  el.style.top = `${top}px`;
  el.style.left = `${left}px`;
}

function hide() {
  if (tip) tip.hidden = true;
}

/** Leaving a nested anchor for its enclosing one hands the tooltip back to it. */
function leave(e: Event) {
  const next = (e as MouseEvent).relatedTarget;
  let el = next instanceof Element ? next : null;
  while (el && !anchors.has(el)) el = el.parentElement;
  if (el instanceof HTMLElement) show(el, anchors.get(el)!);
  else hide();
}

export function tooltip(node: HTMLElement, param: TooltipParam) {
  anchors.set(node, normalize(param));
  const enter = () => show(node, anchors.get(node)!);
  node.addEventListener('mouseenter', enter);
  node.addEventListener('focus', enter);
  node.addEventListener('mouseleave', leave);
  node.addEventListener('blur', hide);
  return {
    update(next: TooltipParam) {
      anchors.set(node, normalize(next));
    },
    destroy() {
      anchors.delete(node);
      node.removeEventListener('mouseenter', enter);
      node.removeEventListener('focus', enter);
      node.removeEventListener('mouseleave', leave);
      node.removeEventListener('blur', hide);
      hide();
    }
  };
}
