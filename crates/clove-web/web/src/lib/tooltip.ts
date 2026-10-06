// A themed hover/focus tooltip. It floats on <body> with fixed positioning so
// list and timeline containers that clip overflow cannot cut it off; one
// element is shared by every trigger.

let tip: HTMLDivElement | null = null;

function element(): HTMLDivElement {
  if (!tip) {
    tip = document.createElement('div');
    tip.className = 'clove-tooltip';
    tip.setAttribute('role', 'tooltip');
    tip.hidden = true;
    document.body.appendChild(tip);
  }
  return tip;
}

function show(anchor: HTMLElement, text: string) {
  const el = element();
  el.textContent = text;
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

export function tooltip(node: HTMLElement, text: string) {
  let current = text;
  const enter = () => show(node, current);
  node.addEventListener('mouseenter', enter);
  node.addEventListener('focus', enter);
  node.addEventListener('mouseleave', hide);
  node.addEventListener('blur', hide);
  return {
    update(next: string) {
      current = next;
    },
    destroy() {
      node.removeEventListener('mouseenter', enter);
      node.removeEventListener('focus', enter);
      node.removeEventListener('mouseleave', hide);
      node.removeEventListener('blur', hide);
      hide();
    }
  };
}
