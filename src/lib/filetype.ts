/// What kind of file a name denotes, for the row's type tile.
///
/// Kept out of App.tsx so the mapping can be tested directly — an extension
/// landing in the wrong bucket is invisible in a screenshot but obvious here.
export type Kind =
  | "video" | "audio" | "archive" | "image" | "doc" | "sheet" | "slides"
  | "book" | "code" | "font" | "subs" | "disc" | "package" | "torrent" | "file";

/// Extension to kind. Listed longest-first where a suffix is ambiguous; the
/// lookup takes the final dotted segment, so "tar.gz" is matched as "gz".
const BY_EXT: Record<string, Kind> = {
  // Video
  mp4: "video", mkv: "video", avi: "video", mov: "video", webm: "video",
  flv: "video", m4v: "video", mpg: "video", mpeg: "video", wmv: "video",
  ts: "video", m2ts: "video", ogv: "video", "3gp": "video", vob: "video",
  m3u8: "video", mpd: "video",

  // Audio
  mp3: "audio", flac: "audio", wav: "audio", aac: "audio", ogg: "audio",
  m4a: "audio", opus: "audio", wma: "audio", aiff: "audio", alac: "audio",
  mid: "audio", midi: "audio",

  // Archives
  zip: "archive", tar: "archive", gz: "archive", xz: "archive", bz2: "archive",
  "7z": "archive", rar: "archive", zst: "archive", lz: "archive", lzma: "archive",
  tgz: "archive", tbz: "archive", cab: "archive", arj: "archive",

  // Images
  png: "image", jpg: "image", jpeg: "image", gif: "image", webp: "image",
  svg: "image", bmp: "image", tiff: "image", tif: "image", ico: "image",
  avif: "image", heic: "image", psd: "image", raw: "image", cr2: "image",

  // Documents
  pdf: "doc", doc: "doc", docx: "doc", odt: "doc", rtf: "doc", txt: "doc",
  md: "doc", tex: "doc", pages: "doc",

  // Spreadsheets
  xls: "sheet", xlsx: "sheet", ods: "sheet", csv: "sheet", tsv: "sheet",
  numbers: "sheet",

  // Presentations
  ppt: "slides", pptx: "slides", odp: "slides", key: "slides",

  // Books
  epub: "book", mobi: "book", azw: "book", azw3: "book", djvu: "book",
  cbz: "book", cbr: "book", fb2: "book",

  // Code and data
  // `ts` is deliberately absent: it is an MPEG transport stream far more often
  // than TypeScript in anything that arrives as a download.
  js: "code", tsx: "code", jsx: "code", py: "code", rs: "code",
  go: "code", java: "code", c: "code", h: "code", cpp: "code", cs: "code",
  rb: "code", php: "code", sh: "code", ps1: "code", sql: "code", html: "code",
  css: "code", json: "code", xml: "code", yml: "code", yaml: "code",
  toml: "code", ini: "code", conf: "code", patch: "code", diff: "code",

  // Fonts
  ttf: "font", otf: "font", woff: "font", woff2: "font", eot: "font",

  // Subtitles
  srt: "subs", vtt: "subs", ass: "subs", ssa: "subs", sub: "subs", sbv: "subs",

  // Disc images
  iso: "disc", img: "disc", dmg: "disc", bin: "disc", cue: "disc",
  nrg: "disc", mdf: "disc",

  // Installers and packages
  exe: "package", msi: "package", appimage: "package", deb: "package",
  rpm: "package", pkg: "package", apk: "package", flatpak: "package",
  snap: "package", jar: "package", whl: "package", crx: "package",
  xpi: "package", dll: "package",

  torrent: "torrent",
};

/// The final dotted segment, lowercased. A name with no dot, or one whose only
/// dot starts it (".bashrc"), has no extension.
export function extensionOf(name: string): string {
  const base = name.split(/[\\/]/).pop() ?? "";
  const dot = base.lastIndexOf(".");
  if (dot <= 0 || dot === base.length - 1) return "";
  return base.slice(dot + 1).toLowerCase();
}

export function kindOf(name: string): Kind {
  return BY_EXT[extensionOf(name)] ?? "file";
}
