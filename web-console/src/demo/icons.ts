const icon = (paths: string, viewBox = "0 0 24 24"): string => `<svg viewBox="${viewBox}" aria-hidden="true" focusable="false"><path d="${paths}" /></svg>`;

export const icons = {
  signal: icon("M4 18.5a8.5 8.5 0 0 1 0-13M8 15.5a4.5 4.5 0 0 1 0-7M12 12h.01M16 8.5a4.5 4.5 0 0 1 0 7M20 5.5a8.5 8.5 0 0 1 0 13"),
  routes: icon("M5 19V5m0 0 4 4m-4-4L2 8m17 11V5m0 0-4 4m4-4 3 3"),
  settings: icon("M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7Zm0-6v3m0 13v3m9-9h-3M6 12H3m15.36-6.36-2.12 2.12M7.76 16.24l-2.12 2.12m12.72 0-2.12-2.12M7.76 7.76 5.64 5.64"),
  archive: icon("M4 7h16v13H4zM3 4h18v3H3zM9 11h6"),
  arrow: icon("M5 12h13m-5-5 5 5-5 5"),
  check: icon("m5 12 4 4L19 6"),
  spark: icon("m12 3 1.6 6.4L20 11l-6.4 1.6L12 19l-1.6-6.4L4 11l6.4-1.6L12 3Z"),
} as const;
