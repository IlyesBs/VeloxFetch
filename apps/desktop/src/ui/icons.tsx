import type { SVGProps } from "react";

// Inline SVG icons (no icon font / remote assets: the CSP allows only 'self').
const PATHS = {
  download: "M12 3v12m0 0-4.5-4.5M12 15l4.5-4.5M5 20h14",
  pause: "M9 5v14M15 5v14",
  play: "M8 5.5v13l11-6.5z",
  x: "M6 6l12 12M18 6L6 18",
  retry: "M4 12a8 8 0 1 0 2.6-5.9M4 4v4.5h4.5",
  trash: "M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3",
  queue: "M4 6h16M4 12h16M4 18h9",
  history: "M3.5 12a8.5 8.5 0 1 0 2.6-6.1M3.5 4.5V9H8M12 8v4.5l3 1.8",
  more: "M5 12h.01M12 12h.01M19 12h.01",
  back: "M15 5l-7 7 7 7",
  copy: "M9 9h10v11H9zM5 15V4h10",
  plus: "M12 5v14M5 12h14",
  check: "M5 12.5l4.5 4.5L19 7",
  alert: "M12 8v5M12 16.5h.01M10.3 3.9L2.4 18a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z",
  sun: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4",
  moon: "M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z",
  gauge: "M12 14l3.5-3.5M4 17a8.5 8.5 0 1 1 16 0",
  network: "M2.5 9a15 15 0 0 1 19 0M5.5 12.5a10 10 0 0 1 13 0M8.5 16a5 5 0 0 1 7 0M12 19.5h.01",
  disk: "M4 7c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3zM4 7v10c0 1.7 3.6 3 8 3s8-1.3 8-3V7M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3",
  layers: "M12 3l9 5-9 5-9-5 9-5zM3 13l9 5 9-5M3 17.5l9 5 9-5",
  power: "M12 3v9M6.3 6.3a8 8 0 1 0 11.4 0",
  calendar: "M4 7a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2zM4 10h16M8 3v4M16 3v4",
  clock: "M12 7v5l3 2M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z",
  folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  external: "M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5",
  search: "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4",
  film: "M4 5h16v14H4zM8 5v14M16 5v14M4 9.5h4M4 14.5h4M16 9.5h4M16 14.5h4",
  music: "M9 18V6l11-2v12M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0zM20 16a3 3 0 1 1-6 0 3 3 0 0 1 6 0z",
  image: "M4 5h16v14H4zM4 16l5-5 4 4 3-3 4 4M15.5 9h.01",
  doc: "M7 3h7l5 5v13H7zM14 3v5h5M10 13h6M10 17h6",
  archive: "M4 4h16v5H4zM5 9v11h14V9M10 13h4",
  app: "M4 5h16v14H4zM4 9h16M7 7h.01M10 7h.01",
  file: "M7 3h7l5 5v13H7zM14 3v5h5",
  lock: "M6 11h12v9H6zM8.5 11V8a3.5 3.5 0 0 1 7 0v3",
  globe: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM3 12h18M12 3c2.5 2.6 3.8 5.6 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.6-3.8-9s1.3-6.4 3.8-9z",
  link: "M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1",
  keyboard: "M3 7h18v10H3zM7 10h.01M11 10h.01M15 10h.01M7 14h10",
  sliders: "M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1M15 4v4M9 10v4M17 16v4",
  chevron: "M9 6l6 6-6 6",
  bolt: "M13 2.5L4.5 13.5H11l-1 8 8.5-11H12l1-8z",
  arrowDown: "M12 5v14m0 0l-5-5m5 5l5-5",
  grid: "M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z",
  language: "M4 5h9M8.5 3v2M6 5c.6 3.2 2.6 5.8 5.5 7.3M11 5c-.8 3.7-3.3 6.6-7 8.2M12.5 21l4-9 4 9M13.8 18h5.4",
  chevronDown: "M6 9l6 6 6-6",
  activity: "M3 12h4l3-8 4 16 3-8h4",
  info: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 11v5M12 8h.01",
  browser: "M3 5h18v14H3zM3 9h18M6 7h.01M9 7h.01",
  clipboard: "M9 3.5h6v3H9zM7.5 5H5v16h14V5h-2.5",
  palette: "M12 3a9 9 0 0 0 0 18c1.1 0 1.5-.8 1.5-1.5 0-1.3-1-1.6-1-2.8 0-.9.7-1.7 1.7-1.7H16a5 5 0 0 0 5-5c0-4-4-7-9-7zM7.5 12h.01M9.5 8h.01M14.5 8h.01",
  broom: "M14 4l6 6M17 7l-7.5 7.5M4 20l3.5-7L11 16.5z",
  moreV: "M12 5h.01M12 12h.01M12 19h.01",
  shield: "M12 3l7.5 3v5.5c0 4.6-3.2 8.3-7.5 9.5-4.3-1.2-7.5-4.9-7.5-9.5V6zM8.8 12.2l2.3 2.3 4.2-4.6",
  trendUp: "M4 16l5.5-5.5 3.5 3.5L20 7M14.5 7H20v5.5",
  trendDown: "M4 8l5.5 5.5 3.5-3.5L20 17M14.5 17H20v-5.5",
  checkCircle: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM8.2 12.3l2.6 2.6 5-5.4",
  activeBox: "M5 4h14a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1zM10 9v6M14 9v6",
  hdd: "M4 13.5L6.5 5h11l2.5 8.5M4 13.5h16V19H4zM16.5 16.25h.01M13.5 16.25h.01",
  inbox: "M12 3v9m0 0L8.5 8.5M12 12l3.5-3.5M4 14h4l1.5 2.5h5L16 14h4v5H4z",
  leaf: "M5 19c0-8 5-13 14-14 0 9-5 14-13 14M5 19c2.5-4 5-6.5 9-9",
  scale: "M12 4v16M8 20h8M5 7h14M5 7l-2.5 6a2.5 2.5 0 0 0 5 0zM19 7l-2.5 6a2.5 2.5 0 0 0 5 0zM12 4.5h.01",
  rocket: "M14 4c3-1 6-1 6-1s0 3-1 6l-6 6-4-4zM9 11l-4 1-2 3 4 1M13 15l-1 4-3 2-1-4M15.5 8.5h.01",
  fold: "M15 6l-6 6 6 6M20 4v16",
  unfold: "M9 6l6 6-6 6M4 4v16",
  gear:
    "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z",
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 16, ...rest }: { name: IconName; size?: number } & SVGProps<SVGSVGElement>) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      {...rest}
    >
      <path d={PATHS[name]} />
    </svg>
  );
}

/**
 * The mark: a neon double "V" (a fast chevron inside the V) on the cyan -> violet gradient. Follows the
 * theme and the Smart throttle colours.
 */
export function Logo({ size = 26 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true" className="logo-mark">
      <defs>
        <linearGradient id="vf-g" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" className="stop-accent" />
          <stop offset="0.55" className="stop-accent-mid" />
          <stop offset="1" className="stop-violet" />
        </linearGradient>
      </defs>
      <path d="M3.5 5.5L16 28 28.5 5.5" fill="none" stroke="url(#vf-g)" strokeWidth="3.4" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M10 5.5l6 10.5 6-10.5" fill="none" stroke="url(#vf-g)" strokeWidth="3.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
