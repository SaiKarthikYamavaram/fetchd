/// The spool mark: a ring broken into four segments — the connections a
/// download is split across.
export function Logo({ size = 18 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={4.2}>
      <circle cx="12" cy="12" r="9.5" strokeDasharray="12.435 2.487" transform="rotate(-90 12 12)" />
    </svg>
  );
}
