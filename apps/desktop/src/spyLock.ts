// The Settings side menu highlights the section on screen as the page scrolls. After a click on a section, the page
// scrolls there smoothly: on the way the highlight would run through every section in between, and near the bottom
// (where the clicked section cannot reach the top) settle on the last one. So the clicked section stays highlighted
// until the scrolling has stopped, or the user scrolls by hand.

export interface SpyLock {
  /** The user chose this section: keep it while the page scrolls there. */
  pin(id: string): void;
  /** A scroll event: `true` while the chosen section must stay highlighted. */
  scrolled(): boolean;
  /** The user scrolls by hand (wheel, touch, keys): follow the page again. */
  release(): void;
}

export function createSpyLock(quietMs = 200): SpyLock {
  let pinned = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const arm = () => {
    clearTimeout(timer);
    timer = setTimeout(() => (pinned = false), quietMs);
  };
  return {
    pin() {
      pinned = true;
      arm();
    },
    scrolled() {
      if (pinned) arm();
      return pinned;
    },
    release() {
      pinned = false;
      clearTimeout(timer);
    },
  };
}
