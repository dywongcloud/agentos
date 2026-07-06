// app/ui/DylanClawLogo.tsx
//
// Inline SVG of the DylanClaw mark + wordmark, identical to the one we
// patched into the @workflow/web dashboard at /. Theme-aware: every fill
// uses currentColor so it picks up var(--foreground) from whichever
// surface it sits on.
//
// Use sizing via the `height` prop (matches the upstream's "h-6 w-auto"
// default of 24px) — the SVG's intrinsic aspect ratio handles width.

import type { CSSProperties } from "react";

type Props = {
  height?: number;
  /** When true, render only the rounded tile mark without the wordmark. */
  markOnly?: boolean;
  style?: CSSProperties;
  title?: string;
};

export default function DylanClawLogo({
  height = 24,
  markOnly = false,
  style,
  title = "DylanClaw",
}: Props) {
  if (markOnly) {
    return (
      <svg
        viewBox="0 0 240 240"
        fill="none"
        xmlns="http://www.w3.org/2000/svg"
        height={height}
        role="img"
        aria-label={title}
        style={{ display: "inline-block", verticalAlign: "middle", ...style }}
      >
        <title>{title}</title>
        <path
          fillRule="evenodd"
          clipRule="evenodd"
          d="M27.5 0H212.5C227.69 0 240 12.31 240 27.5V212.5C240 227.69 227.69 240 212.5 240H27.5C12.31 240 0 227.69 0 212.5V27.5C0 12.31 12.31 0 27.5 0ZM72 60V180H125.36C158.84 180 182 156.84 182 123.36V116.64C182 83.16 158.84 60 125.36 60H72ZM105 90H123.5C141.45 90 152 100.55 152 118.5V121.5C152 139.45 141.45 150 123.5 150H105V90Z"
          fill="currentColor"
        />
      </svg>
    );
  }

  return (
    <svg
      viewBox="0 0 1100 240"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      height={height}
      role="img"
      aria-label={title}
      style={{ display: "inline-block", verticalAlign: "middle", ...style }}
    >
      <title>{title}</title>
      {/* Mark */}
      <path
        fillRule="evenodd"
        clipRule="evenodd"
        d="M27.5 0H212.5C227.69 0 240 12.31 240 27.5V212.5C240 227.69 227.69 240 212.5 240H27.5C12.31 240 0 227.69 0 212.5V27.5C0 12.31 12.31 0 27.5 0ZM72 60V180H125.36C158.84 180 182 156.84 182 123.36V116.64C182 83.16 158.84 60 125.36 60H72ZM105 90H123.5C141.45 90 152 100.55 152 118.5V121.5C152 139.45 141.45 150 123.5 150H105V90Z"
        fill="currentColor"
      />
      {/* Wordmark */}
      <text
        x="280"
        y="172"
        fill="currentColor"
        fontFamily="var(--font-geist-sans), Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif"
        fontSize="148"
        fontWeight="700"
        letterSpacing="-5"
      >
        DylanClaw
      </text>
    </svg>
  );
}
