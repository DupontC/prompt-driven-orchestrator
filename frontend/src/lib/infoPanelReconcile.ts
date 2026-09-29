export interface InfoPanelReconcileInputs {
  /** The active edit-tab id captured at the previous render (the tracker snapshot). */
  prevTabId: string | null;
  /** The active edit-tab id now. */
  nextTabId: string | null;
  /** Whether the Pipeline Info peek overlay is currently open. */
  infoOpen: boolean;
  /** The store's last tab rekey (#774), if any: a rename moved a tab to a new id. */
  rekey?: { from: string; to: string } | null;
}

/**
 * Decide whether the Pipeline Info peek overlay should auto-close because the
 * active edit tab changed (#385).
 *
 * The overlay is toggled from the canvas and given TOP precedence by
 * `rightPaneOwner`, so while open it shadows whatever inspector the right pane
 * would otherwise show. Its content is bound to the ACTIVE tab (that tab's
 * pipeline / run), so when the active tab changes the overlay is describing a
 * tab that is no longer in focus — closing it is the coherent choice.
 *
 * Selecting a different run/library-pipeline/trigger-pipeline, and switching
 * tabs, all move `activeTabId` (run tabs are keyed `__run__<runId>`, so two
 * runs of the SAME pipeline still differ). Reselecting the already-active tab
 * leaves `activeTabId` unchanged (`prevTabId === nextTabId`) → keep it open.
 *
 * A REKEY is not a tab change (#948): a Save that renames the pipeline moves
 * the SAME tab to a new id (`prev → next` matches `rekey`) — the overlay still
 * describes the tab in focus, so it stays open.
 *
 * Keyed on the tab id, NOT on `selection`: the live-run auto-snap effect mutates
 * `selection` (none → node) with no user intent, which would spuriously close.
 */
export function shouldCloseInfoOnTabChange(input: InfoPanelReconcileInputs): boolean {
  if (!input.infoOpen || input.prevTabId === input.nextTabId) return false;
  const rekey = input.rekey;
  return !(rekey && rekey.from === input.prevTabId && rekey.to === input.nextTabId);
}
