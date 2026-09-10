/// A percent ring, e.g. drawn over a thumbnail while it downloads. SVG rather
/// than a CSS conic-gradient mask, so the number sits on top without a second
/// stacking layer.
export function RingProgress({ percent, size = 34 }: { percent: number; size?: number }) {
  const r = (size - 4) / 2;
  const c = 2 * Math.PI * r;
  const offset = c - (Math.min(100, Math.max(0, percent)) / 100) * c;
  return (
    <svg width={size} height={size} className="-rotate-90">
      <circle cx={size / 2} cy={size / 2} r={r} strokeWidth={3} className="fill-none stroke-border" />
      <circle
        cx={size / 2}
        cy={size / 2}
        r={r}
        strokeWidth={3}
        strokeLinecap="round"
        strokeDasharray={c}
        strokeDashoffset={offset}
        className="fill-none stroke-primary transition-[stroke-dashoffset]"
      />
      <text
        x="50%"
        y="50%"
        dy="0.35em"
        textAnchor="middle"
        className="rotate-90 fill-foreground text-[10px] font-medium"
        style={{ transformOrigin: "center" }}
      >
        {Math.round(percent)}
      </text>
    </svg>
  );
}
