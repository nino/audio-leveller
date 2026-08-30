/**
 * This app draws its own window – title bar, traffic lights and all – so it has
 * to know when that window is no longer the front one and go quiet, the way an
 * Aqua window did. What is frontmost here is the browser's window, so the page
 * takes its cue from its own focus and blur.
 *
 * `document.hasFocus()` covers the first paint: a page can be opened in a
 * background tab, or in a window behind the one you are reading.
 */
export function watchWindowActive(): void {
  const set = (active: boolean): void => {
    document.body.classList.toggle("inactive", !active);
  };
  set(document.hasFocus());
  window.addEventListener("focus", () => set(true));
  window.addEventListener("blur", () => set(false));
}
