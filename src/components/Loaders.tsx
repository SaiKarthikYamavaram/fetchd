// CSS micro-loaders. All draw in currentColor, so they inherit the colour of
// whatever they sit in and adapt to theme automatically. Motion is defined in
// App.css under the matching class names.
//
// Original CSS (not copied from any component library): the visual language —
// a ring spinner, bouncing dots, an equaliser of bars, a pulsing dot — is the
// common vocabulary of loading micro-interactions.

type P = { size?: number };

/// Thin rotating arc. For buttons and inline "working" states.
export function Spinner({ size = 16 }: P) {
  return <span className="ldr-spin" style={{ width: size, height: size }} aria-label="loading" />;
}

/// Three dots rising and falling in sequence. For "queued / preparing".
export function Dots() {
  return (
    <span className="ldr-dots" aria-label="waiting">
      <i /><i /><i />
    </span>
  );
}

/// Equaliser bars — reads as "transferring". For an active row's glyph.
export function Bars() {
  return (
    <span className="ldr-bars" aria-label="downloading">
      <i /><i /><i /><i />
    </span>
  );
}

/// Expanding ring that fades — a soft "connecting" heartbeat.
export function Pulse({ size = 16 }: P) {
  return <span className="ldr-pulse" style={{ width: size, height: size }} aria-label="connecting" />;
}
