import { Fragment } from "react";

/**
 * A translated sentence with bold parts marked `<b>…</b>`. Rendered as React elements (never as HTML), so a
 * translation cannot inject markup; any other tag shows as plain text.
 */
export function Rich({ text }: { text: string }) {
  const parts = text.split(/<b>(.*?)<\/b>/g);
  return (
    <>
      {parts.map((p, i) => (i % 2 === 1 ? <b key={i}>{p}</b> : <Fragment key={i}>{p}</Fragment>))}
    </>
  );
}
