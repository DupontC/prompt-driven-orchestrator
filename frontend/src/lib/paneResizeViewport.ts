/**
 * Re-framing the canvas when the right pane folds or unfolds (#949, FP iteration 1).
 *
 * React Flow anchors its viewport on the canvas's top-left corner: when the
 * right pane unfolds, the canvas shrinks from the right and every card keeps its
 * screen position. The graph was fit-viewed on the full-width canvas (the pane is
 * collapsed when a pipeline opens), so its right end — and often the very card
 * the reader just clicked — slides under the pane.
 *
 * The answer, pure so it can be tested without a DOM: pan by half the width
 * change, so the graph keeps its visual centre, then — if a card is selected and
 * still not fully in frame — centre on it. Zoom never changes.
 */

export interface Viewport {
  x: number;
  y: number;
  zoom: number;
}

/** A card's box in flow coordinates. */
export interface FlowRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export function reframeOnPaneResize(
  viewport: Viewport,
  prevWidth: number,
  width: number,
  height: number,
  selected: FlowRect | null,
): Viewport {
  const { zoom } = viewport;
  const next = { x: viewport.x + (width - prevWidth) / 2, y: viewport.y, zoom };
  if (!selected) return next;
  const left = selected.x * zoom + next.x;
  const top = selected.y * zoom + next.y;
  const right = left + selected.width * zoom;
  const bottom = top + selected.height * zoom;
  const inFrame = left >= 0 && top >= 0 && right <= width && bottom <= height;
  if (inFrame) return next;
  return {
    x: width / 2 - (selected.x + selected.width / 2) * zoom,
    y: height / 2 - (selected.y + selected.height / 2) * zoom,
    zoom,
  };
}
